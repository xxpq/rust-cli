//! 先建好、后接上的压缩器：打破组装时的环（README §11 F11，审查 R1-13）。
//!
//! 组装里有三处真递归，都用 base 的 `late::Late`（工作线 F131；以前这里的注释说只有这一处）：
//! - 这里：压缩器的摘要请求要用主线程的工具注册表（与主线程共享缓存前缀，compact.md §3.2）；注册表来自 agents；
//!   agents 的子循环工厂（`LoopFactory`）又要带上压缩器。组装时先把这个句柄交给子循环，建好压缩器后接上一次，
//!   之后只转发；句柄存弱引用（见 [`LateCompactor`]）；
//! - tools 的 `ToolCatalog`：延迟工具清单（上下文的来源）要查主线程注册表，注册表里的 Agent 工具又要用上下文（弱引用，
//!   tools-more.md §3.4）；
//! - context 的任务提醒（`PromptAssembler::bind_tasks`）：agents 要用上下文，任务清单的读者在 agents 建好之后才有
//!   （agents.md §7.4；改成先建 agents 会改附件的顺序）；读者弱引用 agents 的内部状态（agents 的 `TaskLists`）。
//!
//! 三处的回边都是弱引用，强引用只在 `LocalKernel` 一侧：丢掉内核，整张图连同注入的端口一起释放
//! （`tests/kernel_release.rs`）。
//!
//! `LocalKernel::with_extensions` 返回之前三处都已接上，会话与子循环都在那之后才跑。
//!
//! 缺陷修复（R1-13）：原来没接上时 `auto_compact` 静默回 `NotNeeded`（等于悄悄关掉自动压缩），`context_usage`
//! 等有缺省实现的方法没有转发（总是缺省值）。现在每个方法都转发；没接上时返回错误或记 error 日志，不再静默兜底。

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use rust_cli_base::late::Late;
use rust_cli_kernel_api::compaction::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor,
};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::OrchestrationError;
use tokio_util::sync::CancellationToken;

/// 缺陷修复（丢掉的内核不释放，docs/issues/rust-cli-kernel-release/01-dropped-kernel-not-freed.md）：以前这里强持有压缩器。
/// 压缩器的摘要请求用 agents 的注册表，注册表里的 Agent 工具持有 agents 的内部状态，内部状态（子循环工厂、`AgentsPorts`）
/// 又持有这个句柄，成环；丢掉 `LocalKernel` 后整张图连同注入的端口都留在内存里。常驻进程按需组装、丢掉内核时会一直涨。
/// 现在只存弱引用：强引用由 `LocalKernel` 的 `compactor` 字段持有；会话与子循环都在内核的任务作用域里跑，内核活着时总能升级。
#[derive(Default)]
pub(crate) struct LateCompactor(Late<Weak<dyn Compactor>>);

impl LateCompactor {
    pub(crate) fn bind(&self, compactor: &Arc<dyn Compactor>) {
        if self.0.bind(Arc::downgrade(compactor)).is_err() {
            tracing::error!("the compactor was assembled twice; keeping the first");
        }
    }

    fn inner(&self) -> Result<Arc<dyn Compactor>, OrchestrationError> {
        let late = self.0.get().ok_or_else(|| OrchestrationError::Failed {
            message: "the compactor is not assembled yet".to_owned(),
        })?;
        late.upgrade().ok_or_else(|| OrchestrationError::Failed {
            message: "the kernel that owns the compactor was released".to_owned(),
        })
    }
}

#[async_trait]
impl Compactor for LateCompactor {
    fn should_auto_compact(&self, history: &[Message], selection: &ModelSelection) -> bool {
        match self.inner() {
            Ok(inner) => inner.should_auto_compact(history, selection),
            Err(error) => {
                tracing::error!(%error, "auto compaction could not reach the compactor");
                false
            }
        }
    }

    fn context_usage(
        &self,
        history: &[Message],
        selection: &ModelSelection,
    ) -> Option<rust_cli_kernel_api::compaction::ContextOccupancy> {
        match self.inner() {
            Ok(inner) => inner.context_usage(history, selection),
            Err(error) => {
                tracing::error!(%error, "context usage could not reach the compactor");
                None
            }
        }
    }

    fn blocking_limit(
        &self,
        history: &[Message],
        selection: &ModelSelection,
        failure: Option<&str>,
    ) -> Option<String> {
        match self.inner() {
            Ok(inner) => inner.blocking_limit(history, selection, failure),
            Err(error) => {
                tracing::error!(%error, "the blocking limit could not reach the compactor");
                None
            }
        }
    }

    async fn auto_compact(
        &self,
        request: CompactRequest,
        tracking: &mut CompactTracking,
        cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        self.inner()?.auto_compact(request, tracking, cancel).await
    }

    /// 带进度的自动压缩也转发（compact.md §4.1）：不转发时用默认实现，开始事件又回到摘要完成之后。
    async fn auto_compact_with(
        &self,
        request: CompactRequest,
        tracking: &mut CompactTracking,
        cancel: CancellationToken,
        progress: &dyn rust_cli_kernel_api::compaction::CompactProgress,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        self.inner()?
            .auto_compact_with(request, tracking, cancel, progress)
            .await
    }

    async fn compact_metered(
        &self,
        request: CompactRequest,
        cancel: CancellationToken,
        meter: &dyn rust_cli_kernel_api::compaction::CompactMeter,
    ) -> Result<CompactResult, OrchestrationError> {
        self.inner()?.compact_metered(request, cancel, meter).await
    }
}

#[cfg(test)]
#[path = "late_tests.rs"]
mod tests;
