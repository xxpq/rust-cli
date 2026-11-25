//! 建会话幂等（docs/specs/rust-cli/session-facts.md §6，02 §11.2 F）：调用方给的 id 已经有会话时回原 id，不重写会话行、
//! 不起第二个 actor；同 id 的会话种类、父会话或工作区不同时拒绝。
use std::sync::Arc;

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::{CommandId, SessionId};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, CreateSession, EndReason, KernelError,
    Persistence, StartSource,
};
use rust_cli_kernel_api::session_preferences::OpenStart;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta, SessionStore, StoreQueries};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, selection};

use super::*;
use crate::tests::{kernel, workspace};

fn create(id: &str, persistence: Persistence) -> CreateSession {
    CreateSession {
        id: Some(SessionId::new(id)),
        workspace: workspace(),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
        persistence,
        additional_directories: Vec::new(),
        preferences: Default::default(),
    }
}

#[tokio::test]
async fn creating_an_open_session_again_returns_it() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(ScriptedModel::new(Vec::new())));
    let id = kernel
        .create_session(create("sess_1", Persistence::Immediate))
        .await
        .unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    kernel.rename_session(&id, "Kept".into()).await.unwrap();
    let stored = store.facts(&id);
    let again = kernel
        .create_session(create("sess_1", Persistence::Immediate))
        .await
        .unwrap();
    assert_eq!(again, id);
    // 同一个 actor；会话行与它的事实没有重写（以前再建一次覆盖会话行，改的名就没了）。
    assert!(Arc::ptr_eq(
        &first,
        &kernel.open_session(&id).await.unwrap()
    ));
    assert_eq!(store.facts(&id), stored);
    assert_eq!(store.title(&id).as_deref(), Some("Kept"));
}

#[tokio::test]
async fn creating_a_stored_session_again_returns_it_without_opening_it() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(ScriptedModel::new(Vec::new())));
    let id = kernel
        .create_session(create("sess_1", Persistence::Immediate))
        .await
        .unwrap();
    kernel
        .open_session(&id)
        .await
        .unwrap()
        .close(EndReason::Clear)
        .await;
    kernel.rename_session(&id, "Kept".into()).await.unwrap();
    let again = kernel
        .create_session(create("sess_1", Persistence::Deferred))
        .await
        .unwrap();
    assert_eq!(again, id);
    assert!(!kernel.sessions().contains_key(&id), "no actor was started");
    assert_eq!(store.title(&id).as_deref(), Some("Kept"));
}

/// 草稿还没落库：再建一次回同一个草稿（同一个 actor），仍然不落库。
#[tokio::test]
async fn creating_an_open_draft_again_returns_it() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(ScriptedModel::new(Vec::new())));
    let id = kernel
        .create_session(create("sess_draft", Persistence::Deferred))
        .await
        .unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    let again = kernel
        .create_session(create("sess_draft", Persistence::Deferred))
        .await
        .unwrap();
    assert_eq!(again, id);
    assert!(Arc::ptr_eq(
        &first,
        &kernel.open_session(&id).await.unwrap()
    ));
    assert!(store.session_facts(&[id]).await.unwrap().is_empty());
}

/// 同 id 的是别的会话（fork、别的工作区）：拒绝，不改它。
#[tokio::test]
async fn creating_over_a_different_session_is_refused() {
    let store = Arc::new(MemoryStore::default());
    let fork = SessionMeta {
        id: SessionId::new("sess_fork"),
        kind: SessionKind::Fork,
        parent: Some(SessionId::new("sess_parent")),
        workspace: workspace(),
        title: "Fork of x".into(),
        trace: rust_cli_kernel_api::ids::TraceId::new("sess_parent"),
        created_at_ms: 1,
    };
    store
        .create(fork.clone(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let elsewhere = SessionMeta {
        id: SessionId::new("sess_other"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: rust_cli_kernel_api::env::WorkspaceRef {
            root: EnvPath::new("/elsewhere"),
            ..workspace()
        },
        ..fork.clone()
    };
    store
        .create(elsewhere, rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let kernel = kernel(store.clone(), Arc::new(ScriptedModel::new(Vec::new())));
    for id in ["sess_fork", "sess_other"] {
        let refused = kernel
            .create_session(create(id, Persistence::Immediate))
            .await;
        assert!(
            matches!(&refused, Err(KernelError::Exists { session }) if session.as_str() == id),
            "{refused:?}"
        );
    }
    assert_eq!(
        store
            .session_facts(std::slice::from_ref(&fork.id))
            .await
            .unwrap()[0]
            .kind,
        SessionKind::Fork
    );
    assert!(kernel.sessions().is_empty());
}

/// 内核缺省带着 `--add-dir` 的目录的内核（session-dirs.md §3.4）。
fn kernel_with_dirs(store: Arc<MemoryStore>, dirs: &[&str]) -> LocalKernel {
    crate::tests::kernel_with_defaults(
        store,
        Arc::new(ScriptedModel::new(Vec::new())),
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
            additional_directories: dirs.iter().map(|dir| EnvPath::new(*dir)).collect(),
        },
    )
}

/// 起始的附加目录（session-dirs.md §3.1）：内核缺省在前，接上建会话带的；相对路径、在工作区根里的、重复的不收。
#[tokio::test]
async fn a_new_session_starts_with_the_defaults_then_the_requested_directories() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_with_dirs(store, &["/cli"]);
    let id = kernel
        .create_session(CreateSession {
            additional_directories: ["/other", "relative", "/work/sub", "/cli", "/third"]
                .into_iter()
                .map(EnvPath::new)
                .collect(),
            ..create("sess_1", Persistence::Deferred)
        })
        .await
        .unwrap();
    let view = kernel
        .open_session(&id)
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    assert_eq!(
        view.additional_directories,
        ["/cli", "/other", "/third"].map(EnvPath::new)
    );
}

/// 附加目录不落库：冷打开的会话只有内核缺省（session-dirs.md §3.1、待确认 1）。
#[tokio::test]
async fn a_cold_opened_session_has_only_the_defaults() {
    let store = Arc::new(MemoryStore::default());
    let first = kernel_with_dirs(store.clone(), &[]);
    let id = first
        .create_session(CreateSession {
            additional_directories: vec![EnvPath::new("/other")],
            ..create("sess_1", Persistence::Immediate)
        })
        .await
        .unwrap();
    first
        .open_session(&id)
        .await
        .unwrap()
        .close(EndReason::Clear)
        .await;
    let second = kernel_with_dirs(store, &["/cli"]);
    let view = second
        .open_session(&id)
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    assert_eq!(view.additional_directories, [EnvPath::new("/cli")]);
}

/// 冷打开时调用方带的目录（App 里是 Host 给的，session-dirs.md §12.4）：接在内核缺省之后，同建会话的规则（相对的、在工作区根里的、重复的不收）。
#[tokio::test]
async fn a_cold_open_starts_with_the_defaults_then_the_given_directories() {
    let store = Arc::new(MemoryStore::default());
    let first = kernel_with_dirs(store.clone(), &[]);
    let id = first
        .create_session(create("sess_1", Persistence::Immediate))
        .await
        .unwrap();
    first
        .open_session(&id)
        .await
        .unwrap()
        .close(EndReason::Clear)
        .await;
    let second = kernel_with_dirs(store, &["/cli"]);
    let given = ["/given", "relative", "/work/sub", "/cli"].map(EnvPath::new);
    let view = second
        .open_session_with(
            &id,
            StartSource::Resume,
            OpenStart {
                directories: given.to_vec(),
                ..OpenStart::default()
            },
        )
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    assert_eq!(
        view.additional_directories,
        ["/cli", "/given"].map(EnvPath::new)
    );
}

/// 已经打开着：带的目录不起作用（I16：运行实例存在时不刷新，session-dirs.md §3.1）。
#[tokio::test]
async fn directories_given_to_an_open_session_change_nothing() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_with_dirs(store, &[]);
    let id = kernel
        .create_session(CreateSession {
            additional_directories: vec![EnvPath::new("/other")],
            ..create("sess_1", Persistence::Immediate)
        })
        .await
        .unwrap();
    let view = kernel
        .open_session_with(
            &id,
            StartSource::Resume,
            OpenStart {
                directories: vec![EnvPath::new("/given")],
                ..OpenStart::default()
            },
        )
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    assert_eq!(view.additional_directories, [EnvPath::new("/other")]);
}

/// fork 出的会话继承 fork 时父会话的目录（session-dirs.md §12.5）：父会话关了之后才打开子会话（TUI 的 `/fork`）也在；只取一次，
/// 之后再冷打开只有内核缺省。
#[tokio::test]
async fn a_fork_opened_after_its_parent_closed_has_the_parent_directories() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_with_dirs(store, &[]);
    let id = kernel
        .create_session(CreateSession {
            additional_directories: vec![EnvPath::new("/other")],
            ..create("sess_1", Persistence::Immediate)
        })
        .await
        .unwrap();
    let parent = kernel.open_session(&id).await.unwrap();
    let ack = parent
        .send(CommandEnvelope {
            id: CommandId::new("fork-1"),
            command: Command::Fork { at: None },
        })
        .await;
    let Ack::Accepted {
        result: CommandResult::Forked(forked),
    } = ack
    else {
        panic!("{ack:?}")
    };
    parent.close(EndReason::Resume).await;
    let child = kernel
        .open_session_as(&forked.session, StartSource::Fork)
        .await
        .unwrap();
    assert_eq!(
        child.view().await.unwrap().additional_directories,
        [EnvPath::new("/other")]
    );
    child.close(EndReason::Clear).await;
    let again = kernel.open_session(&forked.session).await.unwrap();
    let view = again.view().await.unwrap();
    assert!(view.additional_directories.is_empty());
}
