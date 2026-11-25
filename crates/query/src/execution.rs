//! 一次运行现在用的模型（docs/specs/rust-cli/session-lifecycle.md §10.39）：有执行覆盖、还没退回时
//! 用执行选择并带凭据引用与退回规则；没有覆盖或已经退回时用会话的模型（`QueryRequest.selection`，也是退回目标）。
//!
//! 退回由模型端口判（`ModelError::ExecutionFellBack`，model.md §5.2），这里只换选择，只退一次。「退回了没有」只有 `RunExecution` 里的
//! 一个标记：这里写，Agent 工具派生子代理时读。

use rust_cli_kernel_api::model::{ModelSelection, RequestOptions};
use rust_cli_kernel_api::turn_execution::RunExecution;

pub(crate) struct RunModel {
    /// 会话的模型：没有执行覆盖、退回之后的请求，与压缩的摘要请求（session-lifecycle.md §10.39）用它。
    session: ModelSelection,
    execution: Option<RunExecution>,
}

impl RunModel {
    pub(crate) fn new(session: ModelSelection, execution: Option<RunExecution>) -> Self {
        Self { session, execution }
    }

    /// 这一步的请求用的选择。
    pub(crate) fn selection(&self) -> &ModelSelection {
        self.execution
            .as_ref()
            .and_then(RunExecution::active)
            .map_or(&self.session, |execution| &execution.selection)
    }

    /// 会话的模型（退回目标；压缩用它，执行的一轮也一样）。
    pub(crate) fn session(&self) -> &ModelSelection {
        &self.session
    }

    /// 给这一步的请求写上执行覆盖的凭据引用与退回规则（有规则时端口单次尝试）；没有覆盖或已经退回时什么都不写。
    pub(crate) fn stamp(&self, options: &mut RequestOptions) {
        if let Some(execution) = self.execution.as_ref().and_then(RunExecution::active) {
            options.execution.clone_from(&execution.credential);
            options.fallback.clone_from(&execution.fallback);
        }
    }

    /// 端口判定要退回：之后的请求用会话的模型、不带引用、照常重试。只退一次：退回之后的请求不带规则，端口不会再判退回。
    pub(crate) fn fall_back(&self) {
        if let Some(execution) = &self.execution {
            execution.fall_back();
        }
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
