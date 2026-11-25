//! 归档会话的永久删除（docs/specs/rust-cli/session-facts.md §5.1）：在用的阻止、空闲的关掉再删、范围变了不删、根不在 = 已删完、
//! 别的工作区不删、删行失败是 Incomplete（凭据留着，接着删完）、没有根也没有凭据是 not-found。
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_kernel_api::deletion::{DeletionBlock, DeletionOutcome, Receipt, SessionFile};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, Persistence,
};
use rust_cli_kernel_api::store::{SessionKind, SessionMeta, SessionStore, Store, StoreError};
use rust_cli_kernel_api::testing::{
    MemoryStore, ScriptedModel, StoreCall, StoreHooks, StoreProxy, selection, text_response,
};
use tokio::sync::Notify;

use super::*;
use crate::tests::workspace;

const WAIT: Duration = Duration::from_secs(5);

fn kernel_on(store: Arc<dyn Store>, model: ScriptedModel) -> LocalKernel {
    LocalKernel::new(
        Ports {
            model: Arc::new(model),
            environments: rust_cli_kernel_api::testing::environments(Arc::new(
                rust_cli_kernel_api::testing::UnusedEnvironment,
            )),
            store,
            artifacts: Arc::new(MemoryStore::default()),
            clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(1_000)),
            ids: Arc::new(rust_cli_kernel_api::testing::SequentialIds::default()),
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

/// 落库的根会话与它的子代理（自有后代），根有一个附件文件。
async fn seeded(store: &MemoryStore, kernel: &LocalKernel) -> SessionId {
    let root = kernel
        .create_session(CreateSession {
            id: Some(SessionId::new("sess_root")),
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    store
        .create(
            SessionMeta {
                id: SessionId::new("sess_child"),
                kind: SessionKind::Subagent,
                parent: Some(root.clone()),
                workspace: workspace(),
                title: String::new(),
                trace: TraceId::new("trace_child"),
                created_at_ms: 1,
            },
            rust_cli_kernel_api::testing::no_facts(),
        )
        .await
        .unwrap();
    store.add_file(
        &root,
        SessionFile {
            relative_path: "cli/artifacts/sess_root/a.png".to_owned(),
            name: "a.png".to_owned(),
            artifact: true,
            bytes: 1,
        },
    );
    root
}

/// 打开着、空闲的会话：关掉再删；行与文件都没了，会话表里也没有它。
#[tokio::test]
async fn an_idle_open_session_is_closed_then_deleted() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_on(store.clone(), ScriptedModel::new(Vec::new()));
    let root = seeded(&store, &kernel).await;
    let handle = kernel.open_session(&root).await.unwrap();

    let preview = kernel.deletion_preview(&root, &workspace()).await.unwrap();
    assert_eq!(
        preview.sessions,
        [root.clone(), SessionId::new("sess_child")]
    );
    assert_eq!(preview.blocked, None);
    assert_eq!(preview.deleted_files, 1);

    let result = kernel
        .delete_permanently(&root, &workspace(), &preview.revision, "op")
        .await
        .unwrap();
    assert_eq!(result.outcome, DeletionOutcome::Deleted);
    assert_eq!(result.sessions, preview.sessions);
    assert!(!store.has_row(&root) && !store.has_row(&SessionId::new("sess_child")));
    assert!(store.files_of(&root).is_empty());
    assert!(!kernel.sessions().contains_key(&root), "the actor is gone");
    assert!(handle.view().await.is_err(), "the old handle is closed");

    // 再删一次：凭据说已经删完（同 Node），执行回 deleted
    assert_eq!(store.receipt(&root), Some(Receipt::Deleted));
    let again = kernel.deletion_preview(&root, &workspace()).await.unwrap();
    assert!(again.completed);
    let result = kernel
        .delete_permanently(&root, &workspace(), "stale", "op")
        .await
        .unwrap();
    assert_eq!(result.outcome, DeletionOutcome::Deleted);
}

/// 根不在库里、也没有凭据：同 Node，not-found，什么都不删。
#[tokio::test]
async fn a_missing_root_without_a_receipt_is_not_found() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_on(store.clone(), ScriptedModel::new(Vec::new()));
    let root = SessionId::new("sess_gone");
    let preview = kernel.deletion_preview(&root, &workspace()).await.unwrap();
    assert_eq!(preview.blocked, Some(DeletionBlock::NotFound));
    let result = kernel
        .delete_permanently(&root, &workspace(), &preview.revision, "op")
        .await
        .unwrap();
    assert_eq!(
        result.outcome,
        DeletionOutcome::Blocked(DeletionBlock::NotFound)
    );
}

/// 回合在跑：预检与执行都阻止（PD-D3），不停它，行留着。
#[tokio::test]
async fn a_running_session_blocks_and_keeps_running() {
    let store = Arc::new(MemoryStore::default());
    let gate = Arc::new(Notify::new());
    let model = ScriptedModel::new(vec![text_response(&["done"])]).gated(gate.clone());
    let kernel = kernel_on(store.clone(), model);
    let root = seeded(&store, &kernel).await;
    let handle = kernel.open_session(&root).await.unwrap();
    let mut events = handle.subscribe(Some(0));
    handle
        .send(CommandEnvelope {
            id: CommandId::new("cmd_run"),
            command: Command::Submit {
                text: "go".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    loop {
        let event = tokio::time::timeout(WAIT, events.next())
            .await
            .expect("the turn must start")
            .unwrap();
        if matches!(event.event, KernelEvent::TurnStarted { .. }) {
            break;
        }
    }

    let preview = kernel.deletion_preview(&root, &workspace()).await.unwrap();
    assert_eq!(preview.blocked, Some(DeletionBlock::Running));
    let result = kernel
        .delete_permanently(&root, &workspace(), &preview.revision, "op")
        .await
        .unwrap();
    assert_eq!(
        result.outcome,
        DeletionOutcome::Blocked(DeletionBlock::Running)
    );
    assert!(store.has_row(&root));
    assert!(
        handle.view().await.unwrap().running.is_some(),
        "still running"
    );
    gate.notify_one();
}

/// 确认之后会话变了：旧的 revision 不删（不扩大确认的范围）。
#[tokio::test]
async fn a_changed_scope_is_not_deleted() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_on(store.clone(), ScriptedModel::new(Vec::new()));
    let root = seeded(&store, &kernel).await;
    let preview = kernel.deletion_preview(&root, &workspace()).await.unwrap();
    store.set_updated(&root, 5_000);
    let result = kernel
        .delete_permanently(&root, &workspace(), &preview.revision, "op")
        .await
        .unwrap();
    assert_eq!(
        result.outcome,
        DeletionOutcome::Blocked(DeletionBlock::Changed)
    );
    assert!(store.has_row(&root));
}

/// 根在别的工作区：预检与执行都给 OtherWorkspace，什么都不删。
#[tokio::test]
async fn another_workspace_is_not_deleted() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_on(store.clone(), ScriptedModel::new(Vec::new()));
    let root = seeded(&store, &kernel).await;
    let other = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/elsewhere"),
    };
    let preview = kernel.deletion_preview(&root, &other).await.unwrap();
    assert_eq!(preview.blocked, Some(DeletionBlock::OtherWorkspace));
    let result = kernel
        .delete_permanently(&root, &other, &preview.revision, "op")
        .await
        .unwrap();
    assert_eq!(
        result.outcome,
        DeletionOutcome::Blocked(DeletionBlock::OtherWorkspace)
    );
    assert!(store.has_row(&root));
}

/// 删行失败的存储（开关打开时）。
struct FailingPurge(std::sync::atomic::AtomicBool);

#[async_trait]
impl StoreHooks for FailingPurge {
    async fn before(&self, call: StoreCall<'_>) -> Result<(), StoreError> {
        match call {
            StoreCall::CompleteDeletion(_) if self.0.load(std::sync::atomic::Ordering::SeqCst) => {
                Err(StoreError::Storage {
                    message: "disk is gone".to_owned(),
                })
            }
            _ => Ok(()),
        }
    }
}

/// 文件删了、行没删掉：Incomplete，凭据留着（deleting）；再预检是 incomplete，执行不再比 revision、接着删完（同 Node）。
#[tokio::test]
async fn a_failed_row_delete_is_incomplete_and_resumes_from_the_receipt() {
    let memory = Arc::new(MemoryStore::default());
    let hooks = Arc::new(FailingPurge(std::sync::atomic::AtomicBool::new(true)));
    let store = Arc::new(StoreProxy::new(memory.clone(), hooks.clone()));
    let kernel = kernel_on(store, ScriptedModel::new(Vec::new()));
    let root = seeded(&memory, &kernel).await;
    let preview = kernel.deletion_preview(&root, &workspace()).await.unwrap();
    let result = kernel
        .delete_permanently(&root, &workspace(), &preview.revision, "op")
        .await
        .unwrap();
    assert!(
        matches!(&result.outcome, DeletionOutcome::Incomplete(reason) if reason.contains("disk is gone")),
        "{result:?}"
    );
    assert!(memory.has_row(&root));
    assert!(memory.files_of(&root).is_empty(), "the files went first");
    assert_eq!(memory.receipt(&root), Some(Receipt::Deleting));

    hooks.0.store(false, std::sync::atomic::Ordering::SeqCst);
    let resumed = kernel.deletion_preview(&root, &workspace()).await.unwrap();
    assert!(resumed.incomplete);
    let result = kernel
        .delete_permanently(&root, &workspace(), "the old revision", "op-2")
        .await
        .unwrap();
    assert_eq!(result.outcome, DeletionOutcome::Deleted);
    assert!(!memory.has_row(&root));
    assert_eq!(memory.receipt(&root), Some(Receipt::Deleted));
}
