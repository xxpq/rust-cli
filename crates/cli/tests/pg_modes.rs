//! DB × 实际执行环境矩阵；需独立本机 PG，不能用默认 ignored 掩盖验收。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "集成验收创建隔离工作区、启动执行器并核对实际文件"
)]
mod pg_modes_support;
#[allow(clippy::print_stderr, reason = "复用 SSH 集成测试夹具的诊断函数")]
mod sshd;
use futures_util::StreamExt;
use pg_modes_support::{Mode, Space, database, id};
use rust_cli_kernel_api::ids::{CommandId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, CreateSession, Delivery, EndReason, Kernel,
    KernelEvent, RewindScope, SessionHandle, TurnOutcome,
};
use rust_cli_kernel_api::store::{SessionFilter, SessionStore};
use rust_cli_kernel_api::testing::{ScriptedModel, selection, text_response, tool_use_response};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn input(command: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(command),
        command: Command::Submit {
            text: text.into(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        },
    }
}
async fn run(handle: &dyn SessionHandle, command: &str, text: &str) {
    let revision = handle.view().await.unwrap().revision;
    let mut events = handle.subscribe(Some(revision));
    let ack = handle.send(input(command, text)).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = events.next().await {
            if let KernelEvent::TurnFinished { outcome, .. } = event.event {
                assert_eq!(outcome, TurnOutcome::Success);
                return;
            }
        }
        panic!("session closed without completing the turn");
    })
    .await
    .expect("turn did not complete");
}
fn script(space: &Space, marker: &str) -> Arc<ScriptedModel> {
    let file = space.root.join("written.txt");
    Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "write",
            "Write",
            &json!({"file_path":file,"content":marker}),
        ),
        tool_use_response(
            "command",
            "Bash",
            &json!({"command":"printf 'from-executor' > command.txt; printf 'EXECUTOR:'; pwd","description":"prove execution location"}),
        ),
        tool_use_response("read", "Read", &json!({"file_path":file})),
        text_response(&["finished"]),
    ]))
}
async fn create(kernel: &dyn Kernel, space: &Space) -> Arc<dyn SessionHandle> {
    let session = kernel
        .create_session(CreateSession {
            id: Some(id()),
            workspace: space.workspace.clone(),
            selection: selection(),
            mode: rust_cli_kernel_api::permission::PermissionMode::BypassPermissions,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    kernel.open_session(&session).await.unwrap()
}
async fn exercise(mode: Mode, pg: bool) {
    let db = if pg { Some(database().await) } else { None };
    let space = Space::new(mode, db, "matrix").await;
    let model = script(&space, "first-content");
    let kernel = space.host(model.clone());
    let handle = create(&kernel, &space).await;
    run(handle.as_ref(), "first", "write then read").await;
    assert_eq!(space.file_text("written.txt"), "first-content");
    assert_eq!(space.file_text("command.txt"), "from-executor");
    let loaded = space.observer.load(&id()).await.unwrap();
    assert_eq!(loaded.checkpoints.len(), 1);
    let outputs: Vec<_> = loaded
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(&user.content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => Some((*is_error, content)),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 3, "{outputs:?}");
    assert!(outputs.iter().all(|(error, _)| !error), "{outputs:?}");
    assert_eq!(model.requests().len(), 4);
    assert!(matches!(
        handle.send(input("first", "write then read")).await,
        Ack::Duplicate { .. }
    ));
    if pg {
        assert!(matches!(
            handle.send(input("first", "changed")).await,
            Ack::Rejected { .. }
        ));
    }
    let before = space
        .observer
        .list(SessionFilter {
            workspace: space.workspace.clone(),
            limit: 10,
        })
        .await
        .unwrap()[0]
        .updated_at_ms;
    kernel
        .rename_session(&id(), "renamed".into())
        .await
        .unwrap();
    let after = space
        .observer
        .list(SessionFilter {
            workspace: space.workspace.clone(),
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(after[0].title, "renamed");
    assert_eq!(after[0].updated_at_ms, before);
    let prompt = loaded
        .messages
        .iter()
        .find(|message| matches!(message,Message::User(user) if user.origin==UserOrigin::Human))
        .unwrap()
        .id()
        .clone();
    let fork = handle
        .send(CommandEnvelope {
            id: CommandId::new("fork"),
            command: Command::ForkFromInput {
                input: prompt.clone(),
            },
        })
        .await;
    let Ack::Accepted {
        result: CommandResult::Forked(fork),
    } = fork
    else {
        panic!("{fork:?}")
    };
    let child = fork.session;
    let forked = space.observer.load(&child).await.unwrap();
    assert_eq!(forked.meta.parent, Some(id()));
    assert!(forked.checkpoints.is_empty());
    handle.close(EndReason::Other).await;
    drop(handle);
    drop(kernel);
    let next_model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "next-write",
            "Write",
            &json!({"file_path":space.root.join("second.txt"),"content":"second-owner"}),
        ),
        text_response(&["continued"]),
    ]));
    let next = space.host(next_model.clone());
    let handle = next.open_session(&id()).await.unwrap();
    assert!(next_model.requests().is_empty());
    if pg {
        assert!(matches!(
            next.input_receipt(&id(), &CommandId::new("first"))
                .await
                .unwrap(),
            Some(Ack::Accepted { .. })
        ));
    }
    assert_eq!(handle.view().await.unwrap().meta.title, "renamed");
    let rewind = handle
        .send(CommandEnvelope {
            id: CommandId::new("rewind"),
            command: Command::Rewind {
                target: prompt,
                scope: RewindScope::Code,
            },
        })
        .await;
    assert!(matches!(rewind, Ack::Accepted { .. }), "{rewind:?}");
    assert!(!space.file_exists("written.txt"));
    run(handle.as_ref(), "second", "continue after reopening").await;
    assert_eq!(space.file_text("second.txt"), "second-owner");
    handle.close(EndReason::Other).await;
    next.delete_session(&id()).await.unwrap();
    assert!(next.open_session(&id()).await.is_err());
    assert!(space.observer.load(&id()).await.is_err());
    space.assert_no_executor_session_database();
    space.close().await;
    println!(
        "DB={} mode={mode:?}: files, commands, receipts, fork, checkpoint restore, reopen and delete verified",
        if pg { "pg" } else { "node" }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the PG mode acceptance runner"]
async fn node_local() {
    exercise(Mode::Local, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the PG mode acceptance runner"]
async fn node_remote() {
    exercise(Mode::Remote, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn pg_local() {
    exercise(Mode::Local, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn pg_remote() {
    exercise(Mode::Remote, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG, Docker and ZCODE_TEST_SSH_ARTIFACT"]
async fn pg_ssh() {
    exercise(Mode::Ssh, true).await;
}

async fn tenants(mode: Mode) {
    let db = database().await;
    let a = Space::new(mode, Some(db.clone()), "tenant-a").await;
    let b = Space::new(mode, Some(db.clone()), "tenant-b").await;
    let ma = script(&a, "tenant-a-secret");
    let mb = script(&b, "tenant-b-secret");
    let ka = a.host(ma);
    let kb = b.host(mb);
    let ha = create(&ka, &a).await;
    let hb = create(&kb, &b).await;
    tokio::join!(
        run(ha.as_ref(), "same-command", "tenant-a-secret"),
        run(hb.as_ref(), "same-command", "tenant-b-secret")
    );
    for (space, own, other) in [
        (&a, "tenant-a-secret", "tenant-b-secret"),
        (&b, "tenant-b-secret", "tenant-a-secret"),
    ] {
        let loaded = space.observer.load(&id()).await.unwrap();
        let text = serde_json::to_string(&loaded.messages).unwrap();
        assert!(text.contains(own));
        assert!(!text.contains(other));
        assert_eq!(
            std::fs::read_to_string(space.root.join("written.txt")).unwrap(),
            own
        );
    }
    ha.close(EndReason::Other).await;
    let reopened_model = Arc::new(ScriptedModel::new(vec![text_response(&[
        "reopened continuation",
    ])]));
    let reopened = a.host(reopened_model.clone());
    let shared = reopened.open_session(&id()).await.unwrap();
    reopened
        .rename_session(&id(), "shared title".into())
        .await
        .unwrap();
    run(shared.as_ref(), "reopened-input", "continue shared session").await;
    assert_eq!(
        a.observer.load(&id()).await.unwrap().meta.title,
        "shared title"
    );
    assert_ne!(
        b.observer.load(&id()).await.unwrap().meta.title,
        "shared title"
    );
    let view = shared.view().await.unwrap();
    let first = view
        .messages
        .iter()
        .find(|message| matches!(message, Message::User(user) if user.origin == UserOrigin::Human))
        .unwrap()
        .id()
        .clone();
    let fork = shared
        .send(CommandEnvelope {
            id: CommandId::new("reopened-fork"),
            command: Command::ForkFromInput { input: first },
        })
        .await;
    let Ack::Accepted {
        result: CommandResult::Forked(fork),
    } = fork
    else {
        panic!("reopened fork: {fork:?}")
    };
    assert_eq!(
        a.observer.load(&fork.session).await.unwrap().meta.parent,
        Some(id())
    );
    let created = reopened
        .create_session(CreateSession {
            id: Some(SessionId::new("reopened-created")),
            workspace: a.workspace.clone(),
            selection: selection(),
            mode: rust_cli_kernel_api::permission::PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    assert!(b.observer.load(&created).await.is_err());
    reopened
        .open_session(&created)
        .await
        .unwrap()
        .close(EndReason::Other)
        .await;
    shared.close(EndReason::Other).await;
    ka.delete_session(&created).await.unwrap();
    hb.close(EndReason::Other).await;
    ka.delete_session(&id()).await.unwrap();
    assert!(b.observer.load(&id()).await.is_ok());
    assert!(db.connections() <= 4);
    a.close().await;
    b.close().await;
    println!("PG mode={mode:?}: identical IDs isolated, same user reopened, delete isolated");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn tenants_local() {
    tenants(Mode::Local).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn tenants_remote() {
    tenants(Mode::Remote).await;
}

struct StalledModel(tokio::sync::Notify);
impl rust_cli_kernel_api::model::ModelPort for StalledModel {
    fn stream(
        &self,
        _request: rust_cli_kernel_api::model::ModelRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>,
    > {
        self.0.notify_one();
        futures_util::stream::once(async move {
            cancel.cancelled().await;
            Err(rust_cli_kernel_api::model::ModelError::Cancelled)
        })
        .boxed()
    }
}
async fn crash_recovery(mode: Mode) {
    let space = Space::new(mode, Some(database().await), "crash").await;
    let model = Arc::new(StalledModel(tokio::sync::Notify::new()));
    let first = space.host(model.clone());
    let handle = create(&first, &space).await;
    assert!(matches!(
        handle.send(input("started", "first input")).await,
        Ack::Accepted { .. }
    ));
    tokio::time::timeout(Duration::from_secs(10), model.0.notified())
        .await
        .unwrap();
    assert!(matches!(
        handle.send(input("queued", "queued exactly once")).await,
        Ack::Accepted { .. }
    ));
    assert_eq!(handle.view().await.unwrap().queue.len(), 1);
    // 模拟 Worker 丢失：停止原宿主；执行器和数据库保持独立存活。
    drop(handle);
    drop(first);
    let resumed_model = script(&space, "recovered");
    let second = space.host(resumed_model.clone());
    let handle = second.open_session(&id()).await.unwrap();
    assert_eq!(handle.view().await.unwrap().queue.len(), 1);
    assert!(resumed_model.requests().is_empty());
    assert!(matches!(
        second
            .input_receipt(&id(), &CommandId::new("queued"))
            .await
            .unwrap(),
        Some(Ack::Accepted { .. })
    ));
    run(handle.as_ref(), "continue", "continue now").await;
    let loaded = space.observer.load(&id()).await.unwrap();
    for wanted in ["queued exactly once", "continue now"] {
        let count=loaded.messages.iter().filter(|message|matches!(message,Message::User(user) if user.origin==UserOrigin::Human && user.content.iter().any(|block|matches!(block,ContentBlock::Text{text} if text==wanted)))).count();
        assert_eq!(count, 1, "{wanted}");
    }
    assert_eq!(
        std::fs::read_to_string(space.root.join("written.txt")).unwrap(),
        "recovered"
    );
    assert_eq!(resumed_model.requests().len(), 4);
    assert!(
        space
            .journal
            .as_ref()
            .unwrap()
            .pending(&id())
            .await
            .unwrap()
            .is_empty()
    );
    handle.close(EndReason::Other).await;
    space.assert_no_executor_session_database();
    space.close().await;
    println!(
        "PG mode={mode:?}: crashed Worker, durable queued input and recovery by a new Worker verified"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn crash_local() {
    crash_recovery(Mode::Local).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn crash_remote() {
    crash_recovery(Mode::Remote).await;
}

async fn users_are_isolated(mode: Mode) {
    use rust_cli_pg_store::StoreAccessContext;
    let db = database().await;
    let a = Space::new(mode, Some(db.clone()), "user-a").await;
    let b = Space::new(mode, Some(db), "user-b").await;
    let ka = a.host(script(&a, "alice-only"));
    let kb = b.host(script(&b, "bob-only"));
    let ha = create(&ka, &a).await;
    let hb = create(&kb, &b).await;
    tokio::join!(
        run(ha.as_ref(), "same-command", "alice-only"),
        run(hb.as_ref(), "same-command", "bob-only")
    );
    for (space, own, other) in [
        (&a, "alice-only", "bob-only"),
        (&b, "bob-only", "alice-only"),
    ] {
        let text =
            serde_json::to_string(&space.observer.load(&id()).await.unwrap().messages).unwrap();
        assert!(text.contains(own));
        assert!(!text.contains(other));
    }
    let other =
        a.pg.as_ref()
            .unwrap()
            .bind(
                StoreAccessContext::verified(&format!("{}-impostor", a.user_id), u64::MAX).unwrap(),
                vec![rust_cli_pg_store::StoreProject {
                    id: "project".into(),
                    workspace: a.workspace.clone(),
                }],
                a.clock.clone(),
                a.ids.clone(),
            )
            .unwrap();
    assert!(other.load(&id()).await.is_err());
    ha.close(EndReason::Other).await;
    hb.close(EndReason::Other).await;
    ka.delete_session(&id()).await.unwrap();
    assert!(b.observer.load(&id()).await.is_ok());
    a.close().await;
    b.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn users_local() {
    users_are_isolated(Mode::Local).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PG"]
async fn users_remote() {
    users_are_isolated(Mode::Remote).await;
}
