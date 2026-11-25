use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, KernelEvent, TurnOutcome,
};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
    selection, text_response,
};

use super::*;
use rust_cli_kernel_api::default_model::DefaultModel;

pub(crate) fn kernel(store: Arc<MemoryStore>, model: Arc<ScriptedModel>) -> LocalKernel {
    kernel_with(store, model, selection().into())
}

fn kernel_with(
    store: Arc<MemoryStore>,
    model: Arc<ScriptedModel>,
    default: DefaultModel,
) -> LocalKernel {
    kernel_with_defaults(
        store,
        model,
        Defaults {
            selection: default,
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
            additional_directories: Vec::new(),
        },
    )
}

/// 同 [`kernel`]，另给内核的缺省。
pub(crate) fn kernel_with_defaults(
    store: Arc<MemoryStore>,
    model: Arc<ScriptedModel>,
    defaults: Defaults,
) -> LocalKernel {
    LocalKernel::new(
        Ports {
            model,
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        defaults,
    )
}

pub(crate) fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/work"),
    }
}

#[tokio::test]
async fn a_created_session_runs_a_turn_and_reloads_after_restart() {
    let store = Arc::new(MemoryStore::default());
    let first = kernel(
        store.clone(),
        Arc::new(ScriptedModel::new(vec![text_response(&["hi"])])),
    );
    let id = first
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let session = first.open_session(&id).await.unwrap();
    let again = first.open_session(&id).await.unwrap();
    assert!(Arc::ptr_eq(&session, &again));
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hello".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if let KernelEvent::TurnFinished { outcome, .. } = event.event {
            assert_eq!(outcome, TurnOutcome::Success);
            break;
        }
    }
    drop(first);

    let second = kernel(store, Arc::new(ScriptedModel::new(Vec::new())));
    let reopened = second.open_session(&id).await.unwrap();
    // 一问一答之外，还有上下文这一步宣布的 attachment（模型身份、延迟工具清单（本地 ToolSearch，工作线 F84：
    // 测试替身答不出能力、模型没有卡片）、agent 清单（有 Agent 工具）、剩余 token、日期；内置工具里有 Bash，所以也宣布
    // 署名），它们与对话一起落库、resume 后重放。
    let messages = reopened.view().await.unwrap().messages;
    let dialogue = messages
        .iter()
        .filter(|message| matches!(message, Message::User(_) | Message::Assistant(_)))
        .count();
    assert_eq!(dialogue, 2);
    let sources: Vec<&str> = messages
        .iter()
        .filter_map(|message| match message {
            Message::Attachment(attachment) => Some(attachment.source.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sources,
        [
            "model",
            "deferred_tools_delta",
            "agent_listing_delta",
            "total_tokens_reminder",
            "date",
            "remote_session_change"
        ]
    );
}

/// 复现（R1-1）：以前关闭后会话还留在 `sessions` 表里（actor 活着），再打开拿到的是旧 actor——不跑
/// SessionStart(resume)，按会话的表也不释放。关闭之后它离开内核，再打开是新的 actor。
#[tokio::test]
async fn a_closed_session_leaves_the_kernel_and_reopens_with_a_new_actor() {
    use rust_cli_kernel_api::session_api::{EndReason, StartSource};
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store, Arc::new(ScriptedModel::new(Vec::new())));
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    first.close(EndReason::Resume).await;
    assert!(
        !kernel.sessions().contains_key(&id),
        "a closed session must leave the kernel"
    );
    assert!(first.view().await.is_err(), "the old actor has stopped");
    let again = kernel
        .open_session_as(&id, StartSource::Resume)
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &again));
    again.view().await.unwrap();
}

/// 工作线 F140（performance.md §12.3）：缺省模型定下之前，建会话与开会话都等着（内核唯一的等待点）；定下后照常。
#[tokio::test]
async fn sessions_wait_for_the_default_model_to_settle() {
    let store = Arc::new(MemoryStore::default());
    let id = kernel(store.clone(), Arc::new(ScriptedModel::new(Vec::new())))
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let (settle, default) = DefaultModel::pending();
    let late = Arc::new(kernel_with(
        store,
        Arc::new(ScriptedModel::new(Vec::new())),
        default,
    ));
    // 缺陷修复（测试数据）：两套内核各用一份从 1 起的 `SequentialIds`，以前新建的会话也叫 `sess_1`，和同时在冷打开的那个
    // 撞名。合入 PG 会话库之后，建会话与冷打开共用会话表（同一个 ID 只能有一方占到），哪一方先被唤醒就是哪一方赢，
    // 测试时过时不过。这里要验证的是两个不同的会话都等缺省模型，所以新建的会话给一个不冲突的 ID。
    let creating = tokio::spawn({
        let late = late.clone();
        async move {
            late.create_session(CreateSession {
                id: Some(SessionId::new("sess_late")),
                workspace: workspace(),
                selection: selection(),
                mode: PermissionMode::Default,
                plan: false,
                persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
                additional_directories: Vec::new(),
                preferences: Default::default(),
            })
            .await
        }
    });
    let opening = tokio::spawn({
        let (late, id) = (late.clone(), id.clone());
        async move { late.open_session(&id).await.map(|_| ()) }
    });
    // 单线程运行时：让出几次，两个任务都已跑到各自的等待点。
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(!creating.is_finished() && !opening.is_finished());
    settle.settle(selection());
    creating.await.unwrap().unwrap();
    opening.await.unwrap().unwrap();
}

/// 每轮的工具禁用名单（session-lifecycle.md §10.15，02 §12.10 审定）：这一轮里 ToolSearch 也搜不到名单里的延迟工具——
/// `select:` 点名它时与找不到一样，关键词搜索的结果里没有它（同一族里别的延迟工具照常搜得到）。以前 ToolSearch 只看主线程的
/// 注册表，模型搜得到名字、调用才被拒。
#[tokio::test]
async fn tool_search_does_not_find_a_tool_the_turn_disallows() {
    use rust_cli_kernel_api::input_extras::InputExtras;
    use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent};
    use rust_cli_kernel_api::testing::tool_use_response;
    use rust_cli_kernel_api::tool_names::TOOL_SEARCH;
    use serde_json::json;

    const HIDDEN: &str = "CronCreate";
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "toolu_select",
            TOOL_SEARCH,
            &json!({"query": format!("select:{HIDDEN}")}),
        ),
        tool_use_response("toolu_keywords", TOOL_SEARCH, &json!({"query": "cron"})),
        text_response(&["done"]),
    ]));
    let kernel = kernel(Arc::new(MemoryStore::default()), model);
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "run the job".to_owned(),
                extras: InputExtras {
                    disallowed_tools: vec![HIDDEN.to_owned()],
                    ..InputExtras::default()
                },
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if let KernelEvent::TurnFinished { outcome, .. } = event.event {
            assert_eq!(outcome, TurnOutcome::Success);
            break;
        }
    }
    let messages = session.view().await.unwrap().messages;
    let result = |id: &str| -> Vec<ToolResultContent> {
        messages
            .iter()
            .filter_map(|message| match message {
                Message::User(user) => Some(&user.content),
                _ => None,
            })
            .flatten()
            .find_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } if tool_use_id.as_str() == id => Some(content.clone()),
                _ => None,
            })
            .unwrap()
    };
    let references = |content: &[ToolResultContent]| -> Vec<String> {
        content
            .iter()
            .filter_map(|item| match item {
                ToolResultContent::ToolReference { tool_name } => Some(tool_name.clone()),
                _ => None,
            })
            .collect()
    };
    let selected = result("toolu_select");
    assert!(references(&selected).is_empty(), "{selected:?}");
    assert!(
        matches!(&selected[..], [ToolResultContent::Text { text }]
            if text.starts_with("No matching deferred tools found")),
        "{selected:?}"
    );
    let found = references(&result("toolu_keywords"));
    assert!(found.iter().any(|name| name == "CronList"), "{found:?}");
    assert!(!found.iter().any(|name| name == HIDDEN), "{found:?}");
}

/// 子代理里的 ToolSearch 只搜这个子代理的工具（tools-more.md §3.5，agents.md §4.2 第 5 条）：前台 Explore 没有
/// NotebookEdit（只读 agent 的 disallowedTools），`select:NotebookEdit` 与找不到一样；它自己的延迟工具 WebFetch 照常加载，下一个
/// 请求的工具里有它。修前 ToolSearch 搜主线程注册表的视图，选到 NotebookEdit，引用在组装请求时全部去掉。
#[tokio::test]
async fn a_subagent_tool_search_finds_only_the_subagent_tools() {
    use rust_cli_kernel_api::model::QuerySource;
    use rust_cli_kernel_api::testing::tool_use_response;
    use rust_cli_kernel_api::tool_names::{AGENT, NOTEBOOK_EDIT, TOOL_SEARCH, WEB_FETCH};
    use serde_json::json;

    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "toolu_agent",
            AGENT,
            &json!({"description": "Search tools", "prompt": "Search for the tools.", "subagent_type": "Explore", "run_in_background": false}),
        ),
        tool_use_response(
            "toolu_hidden",
            TOOL_SEARCH,
            &json!({"query": format!("select:{NOTEBOOK_EDIT}")}),
        ),
        tool_use_response(
            "toolu_own",
            TOOL_SEARCH,
            &json!({"query": format!("select:{WEB_FETCH}")}),
        ),
        text_response(&["searched"]),
        text_response(&["done"]),
    ]));
    let kernel = kernel(Arc::new(MemoryStore::default()), model.clone());
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "search in a subagent".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if let KernelEvent::TurnFinished { outcome, .. } = event.event {
            assert_eq!(outcome, TurnOutcome::Success);
            break;
        }
    }
    let subagent: Vec<_> = model
        .requests()
        .into_iter()
        .filter(|request| matches!(request.source, QuerySource::Subagent(_)))
        .collect();
    assert_eq!(subagent.len(), 3);
    // 第二个子代理请求带着 `select:NotebookEdit` 的结果。
    let after_hidden = serde_json::to_string(&subagent[1].messages).unwrap();
    assert!(
        after_hidden.contains("No matching deferred tools found"),
        "{after_hidden}"
    );
    assert!(
        !after_hidden.contains("Tool references removed"),
        "{after_hidden}"
    );
    let tools = |index: usize| -> Vec<String> {
        subagent[index]
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect()
    };
    assert!(!tools(1).iter().any(|name| name == WEB_FETCH));
    assert!(tools(2).iter().any(|name| name == WEB_FETCH));
}
