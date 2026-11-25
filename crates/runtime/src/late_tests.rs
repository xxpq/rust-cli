//! 先建后接的压缩器（R1-13）：没接上时报错，接上后每个方法都转发。

use std::sync::Mutex;

use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::compaction::{CompactMeter, CompactProgress};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::message::CompactTrigger;
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::QuerySource;

use super::*;

/// 只回答用量与“要压缩”的压缩器；带进度的自动压缩只说开始；带计量的压缩只交一次计量。
struct Fixed;

#[async_trait]
impl Compactor for Fixed {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        true
    }

    fn context_usage(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
    ) -> Option<rust_cli_kernel_api::compaction::ContextOccupancy> {
        Some(occupancy())
    }

    fn blocking_limit(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
        failure: Option<&str>,
    ) -> Option<String> {
        Some(format!("blocked {failure:?}"))
    }

    async fn auto_compact_with(
        &self,
        _request: CompactRequest,
        _tracking: &mut CompactTracking,
        _cancel: CancellationToken,
        progress: &dyn CompactProgress,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        progress.started(CompactTrigger::Auto).await;
        Ok(AutoCompactOutcome::NotNeeded)
    }

    async fn compact_metered(
        &self,
        _request: CompactRequest,
        _cancel: CancellationToken,
        meter: &dyn CompactMeter,
    ) -> Result<CompactResult, OrchestrationError> {
        meter.metered(call()).await;
        Err(OrchestrationError::Failed {
            message: "not in this test".to_owned(),
        })
    }
}

/// 记下说过的开始与交来的计量。
#[derive(Default)]
struct Started(Mutex<Vec<CompactTrigger>>, Mutex<Vec<ModelCall>>);

#[async_trait]
impl CompactMeter for Started {
    async fn metered(&self, call: ModelCall) {
        self.1.lock().unwrap().push(call);
    }
}

#[async_trait]
impl CompactProgress for Started {
    async fn started(&self, trigger: CompactTrigger) {
        self.0.lock().unwrap().push(trigger);
    }

    async fn failed(&self, _trigger: CompactTrigger, _message: String) {}
}

fn selection() -> ModelSelection {
    ModelSelection::new("anthropic".to_owned(), GLM_5_3.to_owned())
}

fn request() -> CompactRequest {
    CompactRequest {
        session: SessionId::new("sess_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
        trigger: CompactTrigger::Auto,
        selection: selection(),
        instructions: None,
        messages: Vec::new(),
        mode: None,
        agent: None,
        root: None,
        initial_token_gap: None,
        additional_directories: Vec::new(),
        preferences: Default::default(),
        prompt: Default::default(),
    }
}

/// 旧代码没接上时回 `NotNeeded`，悄悄跳过自动压缩。
#[tokio::test]
async fn an_unassembled_compactor_fails_instead_of_skipping() {
    let late = LateCompactor::default();
    let outcome = late
        .auto_compact(
            request(),
            &mut CompactTracking::default(),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(outcome, Err(OrchestrationError::Failed { .. })));
    assert!(
        late.compact_metered(
            request(),
            CancellationToken::new(),
            &rust_cli_kernel_api::compaction::Unmetered
        )
        .await
        .is_err()
    );
}

/// 旧代码不转发 `context_usage`（总是缺省的 None）。
#[test]
fn an_assembled_compactor_forwards_every_method() {
    let late = LateCompactor::default();
    // 强引用归组装方（内核的 `compactor` 字段）；句柄只存弱引用。
    let owner: Arc<dyn Compactor> = Arc::new(Fixed);
    late.bind(&owner);
    assert!(late.should_auto_compact(&[], &selection()));
    assert_eq!(late.context_usage(&[], &selection()), Some(occupancy()));
    // 阻塞线（compact.md §2.2）同样转发；没有转发时主循环看到的是缺省的“不拦”。
    assert_eq!(
        late.blocking_limit(&[], &selection(), Some("why")),
        Some("blocked Some(\"why\")".to_owned())
    );
    assert_eq!(
        LateCompactor::default().blocking_limit(&[], &selection(), None),
        None
    );
}

/// 带进度的自动压缩也转发（compact.md §4.1）：不转发时走默认实现，压缩器说的开始到不了前端。
#[tokio::test]
async fn an_assembled_compactor_forwards_the_compaction_progress() {
    let late = LateCompactor::default();
    // 强引用归组装方（内核的 `compactor` 字段）；句柄只存弱引用。
    let owner: Arc<dyn Compactor> = Arc::new(Fixed);
    late.bind(&owner);
    let progress = Started::default();
    let outcome = late
        .auto_compact_with(
            request(),
            &mut CompactTracking::default(),
            CancellationToken::new(),
            &progress,
        )
        .await
        .unwrap();
    assert_eq!(outcome, AutoCompactOutcome::NotNeeded);
    assert_eq!(*progress.0.lock().unwrap(), [CompactTrigger::Auto]);
}

/// 丢掉的内核不释放（docs/issues/rust-cli-kernel-release/01-dropped-kernel-not-freed.md）：句柄以前强持有压缩器，与 agents
/// 成环。现在只存弱引用；持有者释放之后不再转发，报错。
#[tokio::test]
async fn a_released_compactor_fails_instead_of_forwarding() {
    let late = LateCompactor::default();
    let owner: Arc<dyn Compactor> = Arc::new(Fixed);
    late.bind(&owner);
    drop(owner);
    assert!(!late.should_auto_compact(&[], &selection()));
    assert_eq!(late.context_usage(&[], &selection()), None);
    assert!(
        late.compact_metered(request(), CancellationToken::new(), &Started::default())
            .await
            .is_err()
    );
}

/// 带计量的压缩也转发（usage.md §2.1.2）：不转发时走默认实现（转给 `compact`），反应式压缩的摘要请求没有用量行。
#[tokio::test]
async fn an_assembled_compactor_forwards_the_summary_metering() {
    let late = LateCompactor::default();
    let owner: Arc<dyn Compactor> = Arc::new(Fixed);
    late.bind(&owner);
    let meter = Started::default();
    let compacted = late
        .compact_metered(request(), CancellationToken::new(), &meter)
        .await;
    assert!(compacted.is_err());
    assert_eq!(*meter.1.lock().unwrap(), [call()]);
}

fn call() -> ModelCall {
    ModelCall::new(
        rust_cli_kernel_api::ids::MessageId::new("msg_summary"),
        QuerySource::Compact,
        selection(),
        1,
        2,
        CallStatus::Completed,
    )
}

fn occupancy() -> rust_cli_kernel_api::compaction::ContextOccupancy {
    rust_cli_kernel_api::compaction::ContextOccupancy {
        used_tokens: 150_000,
        max_tokens: 180_000,
        compact_at: Some(160_000),
        window: None,
    }
}
