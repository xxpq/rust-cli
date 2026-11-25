//! ← 项目页与 `/resume` 页的内核部分（修复线 F29，tui-panels.md §2、tui-manage.md §10）：查询合并实时状态与分支、
//! 改名、删除、只读快照。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::panels::SessionActivity;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelError,
};
use rust_cli_kernel_api::store::{SessionOrder, SessionQuery};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment,
    environments, selection, text_response,
};

use super::*;

fn kernel<E: rust_cli_kernel_api::testing::AllPorts + 'static>(
    store: Arc<MemoryStore>,
    env: Arc<E>,
) -> LocalKernel {
    let artifacts = store.clone();
    kernel_with(
        ScriptedModel::new(vec![text_response(&["done"])]),
        store,
        artifacts,
        env,
    )
}

fn kernel_with<E: rust_cli_kernel_api::testing::AllPorts + 'static>(
    model: ScriptedModel,
    store: Arc<MemoryStore>,
    artifacts: Arc<dyn rust_cli_kernel_api::store::ArtifactStore>,
    env: Arc<E>,
) -> LocalKernel {
    LocalKernel::new(
        Ports {
            model: Arc::new(model),
            environments: environments(env),
            store,
            artifacts,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
            additional_directories: Vec::new(),
        },
    )
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

fn create(id: &str, root: &str) -> CreateSession {
    CreateSession {
        id: Some(SessionId::new(id)),
        workspace: workspace(root),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
        persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
        additional_directories: Vec::new(),
        preferences: Default::default(),
    }
}

fn all() -> SessionQuery {
    SessionQuery {
        environment: EnvironmentId::new("local"),
        root: None,
        order: SessionOrder::Updated,
        limit: 200,
    }
}

/// 登记处：别的进程在跑或等你的会话。
struct Registry(Vec<rust_cli_kernel_api::peers::PeerRecord>);

#[async_trait::async_trait]
impl rust_cli_kernel_api::peers::PeerMessaging for Registry {
    async fn identity(&self) -> Option<rust_cli_kernel_api::peers::PeerIdentity> {
        None
    }
    async fn peers(&self) -> Vec<rust_cli_kernel_api::peers::PeerRecord> {
        self.0.clone()
    }
    async fn send(
        &self,
        _message: rust_cli_kernel_api::peers::PeerOutbound,
    ) -> Result<(), rust_cli_kernel_api::peers::PeerSendError> {
        Ok(())
    }
    async fn receipt(
        &self,
        _to: rust_cli_kernel_api::peers::PeerAddress,
        _msg_id: String,
        _status: rust_cli_kernel_api::peers::ReceiptStatus,
    ) {
    }
    async fn bind(&self, _session: &SessionId) {}
    async fn unbind(&self, _session: &SessionId) {}
    fn signals(
        &self,
        _session: &SessionId,
    ) -> futures_util::stream::BoxStream<'static, rust_cli_kernel_api::peers::PeerSignal> {
        Box::pin(futures_util::stream::pending())
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn set_status(
        &self,
        _session: &SessionId,
        _status: rust_cli_kernel_api::peers::PeerStatus,
    ) {
    }
}

/// 别的进程的记录合进行：状态、时刻、现场与地址都来自登记（tui-cockpit.md §1）。
#[tokio::test]
async fn sessions_live_in_another_process_carry_the_registry() {
    use rust_cli_kernel_api::peers::{PeerAddress, PeerPresence, PeerRecord, PeerStatus};
    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(UnusedEnvironment);
    let earlier = kernel(store.clone(), env.clone());
    earlier
        .create_session(create("elsewhere", "/w/app"))
        .await
        .unwrap();
    earlier
        .create_session(create("closed", "/w/app"))
        .await
        .unwrap();
    let presence = PeerPresence {
        now: Some("Bash  pnpm test".to_owned()),
        ..PeerPresence::default()
    };
    let record = PeerRecord {
        pid: 42,
        name: "app-1".to_owned(),
        address: PeerAddress::from_socket("/peers/42.sock"),
        kind: "interactive".to_owned(),
        status: Some(PeerStatus::Waiting),
        started_at_ms: 0,
        session: Some(SessionId::new("elsewhere")),
        status_since_ms: Some(700),
        presence: presence.clone(),
        tmux: None,
    };
    let kernel = LocalKernel::with_extensions(
        Ports {
            model: Arc::new(ScriptedModel::new(Vec::new())),
            environments: environments(env),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
            additional_directories: Vec::new(),
        },
        crate::ContextConfig::new(crate::Surface::Print),
        crate::CompactConfig::default(),
        Arc::new(rust_cli_agents::MemoryTaskLists::default()),
        crate::Extensions {
            peers: rust_cli_session::PeerDeps {
                port: Arc::new(Registry(vec![record])),
                hold_timeout_ms: 1_000,
                receipts_wake: false,
            },
            ..crate::Extensions::none()
        },
    );
    let found = kernel.project_sessions(all()).await.unwrap().sessions;
    let of = |id: &str| {
        found
            .iter()
            .find(|s| s.record.id.as_str() == id)
            .unwrap_or_else(|| panic!("no {id}"))
    };
    let live = of("elsewhere");
    assert_eq!(live.activity, Some(SessionActivity::NeedsInput));
    let joined = live.presence.as_ref().unwrap();
    assert!(!joined.here);
    assert_eq!(joined.since_ms, Some(700));
    assert_eq!(joined.presence, presence);
    assert_eq!(joined.address, PeerAddress::from_socket("/peers/42.sock"));
    assert_eq!(of("closed").activity, None);
    assert!(of("closed").presence.is_none());
}

/// 两个目录的会话：打开着的有状态，只在库里的没有；每个目录的分支读一次 `.git/HEAD`。
#[tokio::test]
async fn sessions_of_every_project_carry_state_and_branch() {
    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(
        MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
            cwd: EnvPath::new("/w/app"),
            platform: "darwin".to_owned(),
            os_version: String::new(),
            shell: "zsh".to_owned(),
            is_git_repo: true,
            home: None,
            uid: None,
        })
        .with_file("/w/app/.git/HEAD", "ref: refs/heads/main\n")
        // worktree：`.git` 是文件，指向主仓库里的 gitdir。
        .with_file("/w/lib/.git", "gitdir: ../main/.git/worktrees/lib\n")
        .with_file(
            "/w/main/.git/worktrees/lib/HEAD",
            "ref: refs/heads/feature/x\n",
        ),
    );
    let earlier = kernel(store.clone(), env.clone());
    earlier
        .create_session(create("sess_lib", "/w/lib"))
        .await
        .unwrap();
    drop(earlier);
    let current = kernel(store.clone(), env.clone());
    let id = current
        .create_session(create("sess_app", "/w/app"))
        .await
        .unwrap();
    let session = current.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hello".into(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    while let Some(event) = futures_util::StreamExt::next(&mut events).await {
        if matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::TurnFinished { .. }
        ) {
            break;
        }
    }
    let page = current.project_sessions(all()).await.unwrap();
    let find = |id: &str| {
        page.sessions
            .iter()
            .find(|s| s.record.id.as_str() == id)
            .unwrap()
            .clone()
    };
    let app = find("sess_app");
    assert_eq!(app.activity, Some(SessionActivity::Ready));
    assert_eq!(app.last_reply.as_deref(), Some("done"));
    assert_eq!(app.branch.as_deref(), Some("main"));
    assert_eq!(app.record.first_prompt.as_deref(), Some("hello"));
    let lib = find("sess_lib");
    assert_eq!(lib.activity, None);
    assert_eq!(lib.branch.as_deref(), Some("feature/x"));
    // 只看一个目录。
    let only = current
        .project_sessions(SessionQuery {
            root: Some(EnvPath::new("/w/lib")),
            ..all()
        })
        .await
        .unwrap();
    assert_eq!(only.sessions.len(), 1);
    assert_eq!(only.sessions[0].record.id.as_str(), "sess_lib");
}

/// 改名：打开着的经 actor（`TitleChanged`，视图与库都变），没打开的直接写库。删除没打开的：软删除，之后找不到。
/// 删除打开着的会话见 `delete_tests.rs`（session-facts.md §5：以前拒绝，现在先关再删）。
#[tokio::test]
async fn rename_goes_through_the_actor_when_open_and_delete_marks_the_row() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(UnusedEnvironment));
    kernel
        .create_session(create("sess_closed", "/w"))
        .await
        .unwrap();
    let open = kernel
        .create_session(create("sess_open", "/w"))
        .await
        .unwrap();
    let handle = kernel.open_session(&open).await.unwrap();
    // 第一个会话关掉（离开内核）。
    let closed = SessionId::new("sess_closed");
    kernel
        .open_session(&closed)
        .await
        .unwrap()
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    kernel
        .rename_session(&open, "Open one".into())
        .await
        .unwrap();
    assert_eq!(handle.view().await.unwrap().meta.title, "Open one");
    assert_eq!(store.title(&open).as_deref(), Some("Open one"));
    kernel
        .rename_session(&closed, "Closed one".into())
        .await
        .unwrap();
    assert_eq!(store.title(&closed).as_deref(), Some("Closed one"));
    kernel.delete_session(&closed).await.unwrap();
    assert!(store.is_deleted(&closed));
    let missing = kernel.rename_session(&closed, "again".into()).await;
    assert!(
        matches!(missing, Err(KernelError::NotFound { .. })),
        "{missing:?}"
    );
}

/// 只读快照：没打开的从库里冷加载，不打开会话（之后会话表里没有它）。
#[tokio::test]
async fn snapshots_do_not_open_sessions() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(UnusedEnvironment));
    let id = kernel.create_session(create("sess_s", "/w")).await.unwrap();
    kernel
        .open_session(&id)
        .await
        .unwrap()
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    assert!(!kernel.sessions().contains_key(&id));
    let view = kernel.session_snapshot(&id).await.unwrap();
    assert_eq!(view.meta.id, id);
    assert!(view.running.is_none());
    assert!(
        !kernel.sessions().contains_key(&id),
        "a snapshot must not open the session"
    );
    let missing = kernel.session_snapshot(&SessionId::new("sess_x")).await;
    assert!(
        matches!(missing, Err(KernelError::NotFound { .. })),
        "{missing:?}"
    );
}

/// 分支查找只把目录或文件的 `.git` 当仓库：FIFO 等别的种类继续往上（F58：以前把它当文件读，
/// 读不到就没有分支）。
#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_is_passed_over() {
    let env = MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
        cwd: EnvPath::new("/w/app"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: true,
        home: None,
        uid: None,
    })
    .with_special("/w/app/.git")
    .with_file("/w/.git/HEAD", "ref: refs/heads/up\n");
    let found = branch(&env, &EnvironmentId::new("local"), &EnvPath::new("/w/app")).await;
    assert_eq!(found.as_deref(), Some("up"));
}

/// 冷快照带回合记录（session-lifecycle.md §10.1、§10.13）：前端的冷读按它把消息分到回合。以前快照只拿消息，
/// 回合记录丢了，冷读的行都不属于任何回合。
#[tokio::test]
async fn a_cold_snapshot_carries_the_turn_records() {
    use futures_util::StreamExt;
    use rust_cli_kernel_api::session_api::KernelEvent;

    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: None,
        uid: None,
    }));
    let kernel = kernel(store.clone(), env);
    let id = kernel.create_session(create("sess_t", "/w")).await.unwrap();
    let handle = kernel.open_session(&id).await.unwrap();
    let mut events = handle.subscribe(Some(0));
    handle
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hi".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    while let Some(event) = events.next().await {
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    let live = handle.view().await.unwrap();
    assert_eq!(live.turns.len(), 1);
    assert_eq!(live.sources.len(), 1);
    handle
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    let cold = kernel.session_snapshot(&id).await.unwrap();
    assert_eq!(cold.turns, live.turns);
    // 输入的消息与它的命令同样随冷快照（session-lifecycle.md §10.31）：冷读的用户行与活会话带同一个命令
    assert_eq!(cold.sources, live.sources);
}

/// 冷快照带上下文用量（session-lifecycle.md §10.49，19 §47 R-COLD-USAGE）：与打开着的会话 actor 按同一段模型侧的历史与同一个模型算，同值。
/// 以前冷快照不算，由它建的投影（冷读的会话、子代理的子会话）首个快照没有窗口与已用 token，界面的上下文环是空的。
#[tokio::test]
async fn a_cold_snapshot_carries_the_context_usage() {
    use futures_util::StreamExt;
    use rust_cli_kernel_api::session_api::KernelEvent;

    // 回答带用量：已用的 token 从最后一条 assistant 的用量来
    let mut reply = text_response(&["done"]);
    for event in &mut reply {
        if let Ok(rust_cli_kernel_api::model::ModelEvent::MessageDelta { usage, .. }) = event {
            *usage = rust_cli_kernel_api::message::Usage {
                input_tokens: 1_200,
                output_tokens: 30,
                ..Default::default()
            };
        }
    }
    let store = Arc::new(MemoryStore::default());
    let artifacts = store.clone();
    let kernel = kernel_with(
        ScriptedModel::new(vec![reply]),
        store.clone(),
        artifacts,
        Arc::new(UnusedEnvironment),
    );
    let id = kernel.create_session(create("sess_u", "/w")).await.unwrap();
    let handle = kernel.open_session(&id).await.unwrap();
    let mut events = handle.subscribe(Some(0));
    handle
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hi".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    while let Some(event) = events.next().await {
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    let live = handle.view().await.unwrap().context_usage;
    assert!(live.is_some_and(|usage| usage.used_tokens > 0));
    handle
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    let cold = kernel.session_snapshot(&id).await.unwrap();
    assert_eq!(cold.context_usage, live);
}

/// 回退文件的预览（session-lifecycle.md §10.53）：没打开的会话从库里冷加载算，不打开会话；不是可选的输入为 None。
#[tokio::test]
async fn a_cold_rewind_preview_does_not_open_the_session() {
    use futures_util::StreamExt;
    use rust_cli_kernel_api::ids::MessageId;
    use rust_cli_kernel_api::rewind_preview::RewindPreview;
    use rust_cli_kernel_api::session_api::KernelEvent;

    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: None,
        uid: None,
    }));
    let kernel = kernel(store.clone(), env);
    let id = kernel.create_session(create("sess_p", "/w")).await.unwrap();
    let handle = kernel.open_session(&id).await.unwrap();
    let mut events = handle.subscribe(Some(0));
    handle
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hi".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    while let Some(event) = events.next().await {
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    let prompt = handle.view().await.unwrap().messages[0].id().clone();
    let live = kernel.rewind_preview(&id, &prompt).await.unwrap();
    assert_eq!(live, Some(RewindPreview::default()));
    handle
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    assert_eq!(kernel.rewind_preview(&id, &prompt).await.unwrap(), live);
    assert_eq!(
        kernel
            .rewind_preview(&id, &MessageId::new("msg_none"))
            .await
            .unwrap(),
        None
    );
    assert!(
        !kernel.sessions().contains_key(&id),
        "a preview must not open the session"
    );
}

/// 冷快照带工具结果的输出头部（session-lifecycle.md §10.34）：由冷快照建的投影给结果行补上只给界面的头部。以前快照不带，重新打开的会话
/// 只有给模型的文字。
#[tokio::test]
async fn a_cold_snapshot_carries_the_output_heads() {
    use rust_cli_kernel_api::command_output::{CommandOutputHead, ToolOutputHead};
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore};

    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(UnusedEnvironment));
    let id = kernel.create_session(create("sess_h", "/w")).await.unwrap();
    // 会话不开着：快照走冷读
    kernel
        .open_session(&id)
        .await
        .unwrap()
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    let head = ToolOutputHead {
        tool_use: ToolUseId::new("toolu_1"),
        head: CommandOutputHead {
            head: "line-1".to_owned(),
            output_file: "/tmp/zcode-1000/sess_h/tasks/b1.output".to_owned(),
        },
    };
    store
        .commit(
            &id,
            FactBatch {
                command: None,
                facts: vec![Fact::OutputHead { head: head.clone() }],
                turn: None,
            },
        )
        .await
        .unwrap();
    let cold = kernel.session_snapshot(&id).await.unwrap();
    assert_eq!(cold.output_heads, vec![head]);
}

/// 冷快照带工具调用的起止（session-lifecycle.md §10.37）：由冷快照建的投影（子代理的子会话也只经它）给工具行写开工与结束的时刻。以前快照不带。
#[tokio::test]
async fn a_cold_snapshot_carries_the_tool_times() {
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::metering::{CallStatus, ToolCall};
    use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore};

    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(UnusedEnvironment));
    let id = kernel.create_session(create("sess_t", "/w")).await.unwrap();
    // 会话不开着：快照走冷读
    kernel
        .open_session(&id)
        .await
        .unwrap()
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    let call = ToolCall {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Read".to_owned(),
        started_at_ms: 1_000,
        completed_at_ms: 1_250,
        status: CallStatus::Completed,
        output_bytes: 1,
    };
    store
        .commit(
            &id,
            FactBatch {
                command: None,
                facts: vec![Fact::ToolCalled { call: call.clone() }],
                turn: None,
            },
        )
        .await
        .unwrap();
    let cold = kernel.session_snapshot(&id).await.unwrap();
    assert_eq!(cold.tool_times, vec![call.time()]);
}

/// 回合的文件 diff（session-lifecycle.md §10.45）：打开着的问它的 actor，关掉之后从存储冷加载，同一个值；不打开会话；不认识的会话是 NotFound。
#[tokio::test]
async fn turn_file_diffs_are_the_same_open_and_closed() {
    use futures_util::StreamExt;
    use rust_cli_base::line_diff::Hunk;
    use rust_cli_kernel_api::session_api::KernelEvent;

    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: None,
        uid: None,
    }));
    let model = ScriptedModel::new(vec![
        rust_cli_kernel_api::testing::tool_use_response(
            "toolu_1",
            "Write",
            &serde_json::json!({"file_path": "/w/a.txt", "content": "one\ntwo\n"}),
        ),
        text_response(&["done"]),
    ]);
    let artifacts = Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default());
    let kernel = kernel_with(model, store.clone(), artifacts, env);
    let mut request = create("sess_d", "/w");
    request.mode = PermissionMode::AcceptEdits;
    let id = kernel.create_session(request).await.unwrap();
    let handle = kernel.open_session(&id).await.unwrap();
    let mut events = handle.subscribe(Some(0));
    handle
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "write a".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    let mut turn = None;
    while let Some(event) = events.next().await {
        match event.event {
            KernelEvent::TurnStarted { turn: started, .. } => turn = Some(started),
            KernelEvent::TurnFinished { .. } => break,
            _ => {}
        }
    }
    let turn = turn.unwrap();
    let open = kernel.turn_file_diffs(&id, &turn).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (
            open[0].file.change.path.as_str(),
            open[0].file.change.additions,
            open[0].file.change.deletions
        ),
        ("/w/a.txt", 2, 0)
    );
    assert_eq!(
        open[0].hunks,
        [Hunk {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 2,
            lines: vec!["+one".to_owned(), "+two".to_owned()],
        }]
    );
    handle
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    let cold = kernel.turn_file_diffs(&id, &turn).await.unwrap();
    assert_eq!(cold, open);
    assert!(
        !kernel.sessions().contains_key(&id),
        "reading the diffs must not open the session"
    );
    let missing = kernel
        .turn_file_diffs(&SessionId::new("sess_x"), &turn)
        .await;
    assert!(
        matches!(missing, Err(KernelError::NotFound { .. })),
        "{missing:?}"
    );
}

/// 冷快照带清单文件里的任务（agents.md §7.2，K-S9，SMALL-6）：会话没打开时经 agents 的同一个读法，与打开之后的视图同值。
/// 以前冷快照与视图都不带清单，冷读的 `state.plan` 一直是空的。
#[tokio::test]
async fn a_cold_snapshot_carries_the_task_list() {
    use rust_cli_kernel_api::agents::{TaskList, TaskListStore, TaskRecord, TaskStatus};

    let store = Arc::new(MemoryStore::default());
    let lists = Arc::new(rust_cli_agents::MemoryTaskLists::default());
    let kernel = LocalKernel::with_options(
        Ports {
            model: Arc::new(ScriptedModel::new(Vec::new())),
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store.clone(),
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
            additional_directories: Vec::new(),
        },
        crate::ContextConfig::new(crate::Surface::Interactive),
        crate::CompactConfig::default(),
        lists.clone(),
    );
    let id = kernel.create_session(create("sess_t", "/w")).await.unwrap();
    let task = TaskRecord {
        id: "1".to_owned(),
        subject: "Write the spec".to_owned(),
        description: String::new(),
        active_form: None,
        owner: None,
        status: TaskStatus::InProgress,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
        metadata: None,
    };
    let list = TaskList {
        high_water_mark: 1,
        tasks: vec![task],
    };
    lists.save(&id, &list).await.unwrap();
    let cold = kernel.session_snapshot(&id).await.unwrap();
    assert_eq!(cold.tasks, list.tasks);
    let open = kernel.open_session(&id).await.unwrap();
    assert_eq!(open.view().await.unwrap().tasks, cold.tasks);
}
