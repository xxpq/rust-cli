//! 删除打开着的会话（docs/specs/rust-cli/session-facts.md §5）：先关掉它（中止在跑的回合），再软删除；草稿关掉就是删掉；
//! 删除期间打开它的调用等着，醒来看到 NotFound。以前打开着的会话拒绝删除（`The session is open in this window.`）。
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::{CreateSession, Kernel, KernelError, Persistence};
use rust_cli_kernel_api::store::{StoreError, StoreQueries};
use rust_cli_kernel_api::testing::{
    MemoryStore, ScriptedModel, StoreCall, StoreHooks, StoreProxy, selection,
};
use tokio::sync::Notify;

use super::*;
use crate::tests::workspace;

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

fn kernel_on(store: Arc<dyn rust_cli_kernel_api::store::Store>) -> LocalKernel {
    LocalKernel::new(
        Ports {
            model: Arc::new(ScriptedModel::new(Vec::new())),
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

#[tokio::test]
async fn an_open_session_is_closed_then_deleted() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_on(store.clone());
    let id = kernel
        .create_session(create("sess_open", Persistence::Immediate))
        .await
        .unwrap();
    let handle = kernel.open_session(&id).await.unwrap();
    let mut changes = store.session_fact_changes();
    kernel.delete_session(&id).await.unwrap();
    assert!(store.is_deleted(&id));
    assert!(!kernel.sessions().contains_key(&id), "the actor is gone");
    assert!(handle.view().await.is_err(), "the old handle is closed");
    let deleted = changes.next().await.unwrap();
    assert_eq!(deleted.id, id);
    assert!(deleted.deleted_at_ms.is_some());
    let reopened = kernel.open_session(&id).await;
    assert!(
        matches!(reopened, Err(KernelError::NotFound { .. })),
        "{:?}",
        reopened.err()
    );
}

/// 草稿没有落库：删除关掉它就够了，库里与事实里都没有它。
#[tokio::test]
async fn deleting_an_open_draft_abandons_it() {
    let store = Arc::new(MemoryStore::default());
    let mut changes = store.session_fact_changes();
    let kernel = kernel_on(store.clone());
    let id = kernel
        .create_session(create("sess_draft", Persistence::Deferred))
        .await
        .unwrap();
    kernel.delete_session(&id).await.unwrap();
    assert!(!kernel.sessions().contains_key(&id));
    assert!(store.facts(&id).is_empty());
    assert!(changes.next().now_or_never().is_none());
    let missing = kernel.delete_session(&id).await;
    assert!(
        matches!(missing, Err(KernelError::NotFound { .. })),
        "{missing:?}"
    );
}

/// 软删除停住的存储（测试放行之前，删除占着会话表的条目）。
struct HeldDelete {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl StoreHooks for HeldDelete {
    async fn before(&self, call: StoreCall<'_>) -> Result<(), StoreError> {
        if let StoreCall::Delete(_) = call {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// 关掉之后、软删除之前打开它：等删除放开条目，冷加载看到 NotFound（不会在删掉的会话上起一个新的 actor）。
#[tokio::test]
async fn an_open_during_the_delete_waits_and_finds_nothing() {
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let hooks = HeldDelete {
        entered: entered.clone(),
        release: release.clone(),
    };
    let store = Arc::new(StoreProxy::new(Arc::new(MemoryStore::default()), hooks));
    let kernel = Arc::new(kernel_on(store));
    let id = kernel
        .create_session(create("sess_open", Persistence::Immediate))
        .await
        .unwrap();
    let deleting = tokio::spawn({
        let (kernel, id) = (kernel.clone(), id.clone());
        async move { kernel.delete_session(&id).await }
    });
    // 守卫：旧代码在关闭之前就拒绝，存储停不住（不等到放行就失败，而不是卡住）。
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("the delete reaches the store");
    let opening = tokio::spawn({
        let (kernel, id) = (kernel.clone(), id.clone());
        async move { kernel.open_session(&id).await.map(|_| ()) }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(!opening.is_finished(), "the open waits for the delete");
    release.notify_one();
    deleting.await.unwrap().unwrap();
    let opened = opening.await.unwrap();
    assert!(
        matches!(opened, Err(KernelError::NotFound { .. })),
        "{opened:?}"
    );
}
