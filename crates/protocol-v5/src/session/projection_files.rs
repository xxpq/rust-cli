//! 投影里 ExitPlanMode 行的计划（protocol-v5.md §42.1；从 projection.rs 分出，800 行上限）：活会话取 `ToolDone` 的 display，由视图建时取视图的 `plans`，
//! 都记进投影的 `plans`；写这一行的每一处（`MessageDone` 的工具调用、结局）按它带上，所以 assistant 的 `MessageDone` 晚于 `ToolDone` 时也不丢。
//! 回合头的文件改动在回合结束的那一个 put 里写（`finished` 与 `recorded_end`，`rows::set_files`）。

use rust_cli_kernel_api::plan::ShownPlan;
use rust_cli_kernel_api::tool::ToolOutput;

use super::*;

/// 由视图建时的计划：视图的 `plans`，按调用 id。
pub(super) fn plans(view: &SessionView) -> BTreeMap<String, ShownPlan> {
    view.plans
        .iter()
        .map(|plan| (plan.tool_use.as_str().to_owned(), plan.clone()))
        .collect()
}

impl Projection {
    /// `ToolDone` 带来的计划：记下（之后的 `result` 与重写这一行时带上）。
    pub(super) fn plan_shown(&mut self, tool_use: &ToolUseId, output: &ToolOutput) {
        if let Some(plan) = ShownPlan::of(tool_use, output) {
            self.plans.insert(tool_use.as_str().to_owned(), plan);
        }
    }

    /// 记过计划的调用：这一行的 `input` 带上它。
    pub(super) fn with_plan(&self, id: &str, fields: &mut Fields) {
        if let Some(plan) = self.plans.get(id) {
            rows::set_plan(fields, plan);
        }
    }
}

#[cfg(test)]
#[path = "projection_files_tests.rs"]
mod tests;
