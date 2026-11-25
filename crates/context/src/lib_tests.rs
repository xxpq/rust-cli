use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::side_request::SideSource;
use rust_cli_kernel_api::testing::{FixedClock, SequentialIds, UnusedEnvironment};

use super::*;

fn request(source: QuerySource) -> ContextRequest {
    ContextRequest::new(
        SessionId::new("s"),
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        source,
        PermissionMode::Default,
        ModelSelection::new("p".to_owned(), rust_cli_base::models::GLM_5_3.to_owned()),
    )
}

fn assembler() -> PromptAssembler {
    let env = Arc::new(UnusedEnvironment);
    PromptAssembler::new(
        ContextPorts {
            info: env.clone(),
            files: env.clone(),
            shell: env.clone(),
            git: env,
            clock: Arc::new(FixedClock(1_792_224_000_000)),
            ids: Arc::new(SequentialIds::default()),
            listings: Arc::new(NoListings),
        },
        ContextConfig::new(Surface::Print),
    )
}

/// 回合从一条人类输入开始。
fn prompt() -> Vec<Message> {
    vec![Message::User(rust_cli_kernel_api::message::UserMessage {
        id: rust_cli_kernel_api::ids::MessageId::new("msg_u"),
        content: vec![rust_cli_kernel_api::message::ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: rust_cli_kernel_api::message::UserOrigin::Human,
    })]
}

#[tokio::test]
async fn the_main_thread_gets_its_announcements_without_environment_facts() {
    let built = assembler()
        .step(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    assert_eq!(built.system.len(), 3);
    let sources: Vec<&str> = built
        .attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect();
    // 没有 Bash 工具时不宣布署名。
    assert_eq!(sources, ["model", "total_tokens_reminder", "date"]);
    assert!(
        built
            .attachments
            .iter()
            .all(|item| item.text.starts_with("<system-reminder>\n"))
    );
    assert_eq!(
        built.attachments[2].text,
        "<system-reminder>\nToday's date is 2026-10-17.\n</system-reminder>"
    );
}

#[tokio::test]
async fn the_attribution_needs_the_bash_tool() {
    let mut with_bash = request(QuerySource::MainThread);
    with_bash.tool_names = vec!["Bash".to_owned()];
    let built = assembler().build(with_bash, &[]).await.unwrap();
    assert_eq!(
        built.attachments.last().map(|item| item.source.as_str()),
        Some("remote_session_change")
    );
}

#[tokio::test]
async fn other_query_sources_get_no_announcements() {
    let built = assembler()
        .build(request(QuerySource::Side(SideSource::SessionTitle)), &[])
        .await
        .unwrap();
    assert!(built.attachments.is_empty());
    assert_eq!(built.system.len(), 3);
    // 摘要请求只补消息里没有的宣布（context.md §4.1）：这里没有执行环境事实，只有日期。
    let built = assembler()
        .build(request(QuerySource::Compact), &[])
        .await
        .unwrap();
    let sources: Vec<&str> = built
        .attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect();
    assert_eq!(sources, ["date"]);
}

/// F10（query.md §1.1）：主线程的一步拆成两个时点。当步的 attachment（模型、剩余 token…）
/// 只在 `turn_attachments` 里，`build` 只给上下文宣布（日期、署名）；摘要与旁路请求没有当步的
/// attachment（子代理见 `subagent_tests.rs`，F16）。旧代码在 `build` 里一次给出全部，`turn_attachments` 是缺省的空表。
#[tokio::test]
async fn the_main_thread_step_splits_turn_attachments_from_announcements() {
    let assembler = assembler();
    let turn = assembler
        .turn_attachments(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    let turn: Vec<&str> = turn.iter().map(|item| item.source.as_str()).collect();
    assert_eq!(turn, ["model", "total_tokens_reminder"]);
    let built = assembler
        .build(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    let announced: Vec<&str> = built
        .attachments
        .iter()
        .map(|item| item.source.as_str())
        .collect();
    assert_eq!(announced, ["date"]);
    for source in [
        QuerySource::Compact,
        QuerySource::Side(SideSource::SessionTitle),
    ] {
        let turn = assembler
            .turn_attachments(request(source), &prompt())
            .await
            .unwrap();
        assert!(turn.is_empty());
    }
}

/// 剩余 token 跨压缩（工作线 F82）：压缩前这次输入里用掉的不因压缩归还，压缩之后的工具轮照样从输入那一刻算。
/// 以前只看边界之后的历史，压缩之后回到满额。
#[tokio::test]
async fn the_tokens_left_do_not_reset_at_a_compaction() {
    use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
    use rust_cli_kernel_api::message::{
        AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, ToolResultContent, Usage,
        UserMessage, UserOrigin,
    };
    let round = |index: &str, input_tokens: u64| {
        let assistant = MessageId::new(format!("a{index}"));
        [
            Message::Assistant(AssistantMessage {
                id: assistant.clone(),
                content: vec![ContentBlock::ToolUse {
                    id: ToolUseId::new(format!("t{index}")),
                    name: "Read".to_owned(),
                    input: serde_json::json!({}),
                }],
                model: "m".to_owned(),
                stop_reason: None,
                usage: Some(Usage {
                    input_tokens,
                    output_tokens: 16,
                    ..Usage::default()
                }),
            }),
            Message::User(UserMessage {
                id: MessageId::new(format!("r{index}")),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new(format!("t{index}")),
                    content: vec![ToolResultContent::Text {
                        text: "ok".to_owned(),
                    }],
                    is_error: false,
                }],
                origin: UserOrigin::ToolResult { assistant },
            }),
        ]
    };
    let mut history = prompt();
    history.extend(round("1", 990_000));
    history.push(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new("b"),
            trigger: CompactTrigger::Auto,
            pre_tokens: 990_016,
            summary: MessageId::new("s"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
    )));
    history.push(Message::User(UserMessage {
        id: MessageId::new("s"),
        content: vec![ContentBlock::Text {
            text: "summary".to_owned(),
        }],
        origin: UserOrigin::CompactSummary,
    }));
    history.extend(round("2", 10));
    let turn = assembler()
        .turn_attachments(request(QuerySource::MainThread), &history)
        .await
        .unwrap();
    let tokens = turn
        .iter()
        .find(|item| item.source == "total_tokens_reminder")
        .unwrap();
    // 压缩前的 990,016（990,000 + 16）滚进累计，加上压缩之后这一轮的 10 + 16。
    assert!(
        tokens.text.contains("14009958 tokens left"),
        "{}",
        tokens.text
    );
}

/// system 的输入来自这一步会话钉住的那一份（context.md §15.3）：追加的文字是会话段最后一段，`!` 提示看钉住时的前端能力
/// （TUI 有，App 与 `-p` 没有，§15.5）。以前取内核级的 `ContextConfig` 与 `Surface`，续写换了参数时 system 跟着变。
#[tokio::test]
async fn the_system_follows_the_session_pin() {
    use rust_cli_kernel_api::session_prompt::{SessionPrompt, SystemPin};
    let pinned = |shell_escape: bool| ContextRequest {
        tool_names: vec![SKILL_TOOL.to_owned()],
        prompt: Arc::new(SessionPrompt::default().with_system(SystemPin {
            custom: None,
            append: Some("Always end with DONE.".to_owned()),
            shell_escape,
        })),
        ..request(QuerySource::MainThread)
    };
    let terminal = assembler().build(pinned(true), &prompt()).await.unwrap();
    let session = &terminal.system[2].text;
    assert!(session.ends_with("\n\nAlways end with DONE."), "{session}");
    assert!(session.contains("! <command>"), "{session}");
    let app = assembler().build(pinned(false), &prompt()).await.unwrap();
    assert!(!app.system[2].text.contains("! <command>"));
    assert!(app.system[2].text.ends_with("\n\nAlways end with DONE."));
}

/// 前端上下文是主线程的当步附件（context.md §15.6）：内容由前端层给，子代理没有；没有前端层（测试、嵌入）时不发。增量看最近一次
/// 压缩边界之后最后一条的 `data`（前端层比较）。
#[tokio::test]
async fn the_frontend_context_is_for_the_main_thread() {
    use rust_cli_kernel_api::message::CompactBoundary;
    use rust_cli_kernel_api::testing::EchoFrontend;

    let with_prompt = || assembler().with_frontend_prompt(Arc::new(EchoFrontend));
    let sources = |items: Vec<AttachmentMessage>| -> Vec<String> {
        items.into_iter().map(|item| item.source).collect()
    };
    let main = with_prompt()
        .turn_attachments(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    let shown = main
        .iter()
        .find(|item| item.source == "frontend_context")
        .unwrap()
        .clone();
    assert_eq!(
        shown.text,
        "<system-reminder>\nfrontend of s\n</system-reminder>"
    );
    assert_eq!(
        shown.role(),
        rust_cli_kernel_api::message::AttachmentRole::System
    );
    let child = with_prompt()
        .turn_attachments(
            request(QuerySource::Subagent("general-purpose".to_owned())),
            &prompt(),
        )
        .await
        .unwrap();
    assert!(!sources(child).contains(&"frontend_context".to_owned()));
    let none = assembler()
        .turn_attachments(request(QuerySource::MainThread), &prompt())
        .await
        .unwrap();
    assert!(!sources(none).contains(&"frontend_context".to_owned()));
    // 宣布过的（同一个 `data`）不再发；压缩边界之前宣布的不算
    let mut history = vec![Message::Attachment(shown)];
    history.extend(prompt());
    let again = with_prompt()
        .turn_attachments(request(QuerySource::MainThread), &history)
        .await
        .unwrap();
    assert!(!sources(again).contains(&"frontend_context".to_owned()));
    let boundary = Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: rust_cli_kernel_api::ids::MessageId::new("msg_b"),
        trigger: rust_cli_kernel_api::message::CompactTrigger::Manual,
        pre_tokens: 0,
        summary: rust_cli_kernel_api::ids::MessageId::new("msg_s"),
        preserved: None,
        discovered_tools: Vec::new(),
    }));
    history.insert(1, boundary);
    let after = with_prompt()
        .turn_attachments(request(QuerySource::MainThread), &history)
        .await
        .unwrap();
    assert!(sources(after).contains(&"frontend_context".to_owned()));
}

/// `@` 插件的引用（context.md §15.11）：前端层从人类键入的输入里解析出 id，内核按 id 查能力，前端层写提醒；user 角色。没有
/// 前端层、没有引用时不查插件目录。
#[tokio::test]
async fn plugin_references_are_parsed_by_the_frontend_and_looked_up_by_the_kernel() {
    use std::sync::Mutex;

    use rust_cli_kernel_api::listing::PluginCapabilities;
    use rust_cli_kernel_api::message::{AttachmentRole, ContentBlock, UserMessage, UserOrigin};
    use rust_cli_kernel_api::testing::EchoFrontend;

    /// 记下被问到的 id；认识 `kit@m`。
    #[derive(Default)]
    struct Directory(Mutex<Vec<Vec<String>>>);

    #[async_trait]
    impl ListingSource for Directory {
        async fn listings(
            &self,
            _request: &ContextRequest,
        ) -> Result<Listings, OrchestrationError> {
            Ok(Listings::default())
        }

        async fn plugin_references(
            &self,
            _request: &ContextRequest,
            ids: &[String],
        ) -> Result<Vec<PluginCapabilities>, OrchestrationError> {
            use rust_cli_base::sync::LockUnpoisoned;
            self.0.lock_unpoisoned().push(ids.to_vec());
            Ok(ids
                .iter()
                .filter(|id| *id == "kit@m")
                .map(|id| PluginCapabilities {
                    id: id.clone(),
                    ..PluginCapabilities::default()
                })
                .collect())
        }
    }

    let typed = |text: &str| {
        vec![Message::User(UserMessage {
            id: rust_cli_kernel_api::ids::MessageId::new("msg_u"),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            origin: UserOrigin::Human,
        })]
    };
    let directory = Arc::new(Directory::default());
    let env = Arc::new(UnusedEnvironment);
    let assembler = |with_prompt: bool| {
        let assembler = PromptAssembler::new(
            ContextPorts {
                info: env.clone(),
                files: env.clone(),
                shell: env.clone(),
                git: env.clone(),
                clock: Arc::new(FixedClock(1_792_224_000_000)),
                ids: Arc::new(SequentialIds::default()),
                listings: directory.clone(),
            },
            ContextConfig::new(Surface::Print),
        );
        if with_prompt {
            assembler.with_frontend_prompt(Arc::new(EchoFrontend))
        } else {
            assembler
        }
    };
    let found = assembler(true)
        .turn_attachments(
            request(QuerySource::MainThread),
            &typed("use plugin:kit@m and plugin:gone@m"),
        )
        .await
        .unwrap();
    let reference = found
        .iter()
        .find(|item| item.source == "plugin_reference")
        .unwrap();
    assert_eq!(
        reference.text,
        "<system-reminder>\nreferenced kit@m\n</system-reminder>"
    );
    assert_eq!(reference.role(), AttachmentRole::User);
    // 没有引用、没有前端层：不问插件目录
    for (with_prompt, text) in [(true, "nothing here"), (false, "use plugin:kit@m")] {
        let none = assembler(with_prompt)
            .turn_attachments(request(QuerySource::MainThread), &typed(text))
            .await
            .unwrap();
        assert!(none.iter().all(|item| item.source != "plugin_reference"));
    }
    use rust_cli_base::sync::LockUnpoisoned;
    assert_eq!(
        *directory.0.lock_unpoisoned(),
        [vec!["kit@m".to_owned(), "gone@m".to_owned()]]
    );
}
