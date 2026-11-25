//! Bash 结果里只给界面的输出头部（docs/specs/rust-cli/shell-permissions.md §1.3.1）：输出超过内联上限时，给模型的是
//! `<persisted-output>` 预览（输出文件的位置与前 2KB），界面要显示输出的头部。写在工具结果的 `display[DISPLAY_KEY]`，
//! 与 [`crate::background::DISPLAY_KEY`]、[`crate::agents::DISPLAY_KEY`]、[`crate::checkpoint::DISPLAY_KEY`] 并列。

use serde::{Deserialize, Serialize};

use crate::ids::ToolUseId;
use crate::store::Fact;
use crate::tool::ToolOutput;

/// 工具结果的 `display` 里输出头部的键。
pub const DISPLAY_KEY: &str = "commandOutput";

/// 超过内联上限的结果（成功与失败，shell-permissions.md §1.3.1）给界面的那一份。只在这个情形下有：头部之外的输出都在输出文件里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutputHead {
    /// 输出文件的头部（内联上限以内，去掉开头的空白行与结尾空白；模型看到的 2KB 预览取自它）。
    pub head: String,
    /// 执行命令的那台机器上的输出文件。
    pub output_file: String,
}

impl CommandOutputHead {
    /// 工具结果里的输出头部；没有（或读不出）时为 None。
    pub fn of(output: &ToolOutput) -> Option<Self> {
        serde_json::from_value(output.display.get(DISPLAY_KEY)?.clone()).ok()
    }

    /// 写进工具结果的 `display`（保留别的键）。
    pub fn attach(self, mut output: ToolOutput) -> ToolOutput {
        output.display[DISPLAY_KEY] = serde_json::to_value(self).unwrap_or_default();
        output
    }

    /// 只有它的 `display`：失败的结果随 [`crate::tool::ToolError::FailedWithDisplay`] 交给执行管线的那一份。
    pub fn display(self) -> serde_json::Value {
        let mut display = serde_json::Value::Null;
        display[DISPLAY_KEY] = serde_json::to_value(self).unwrap_or_default();
        display
    }

    /// 工具结果里有头部时与它同批提交的事实（`RunLedger` 在工具结束时取出；成功与失败的结果都看）。
    pub(crate) fn fact(tool_use: &ToolUseId, output: &ToolOutput) -> Option<Fact> {
        ToolOutputHead::of(tool_use, output).map(|head| Fact::OutputHead { head })
    }
}

/// 一次调用的输出头部：落库与冷读的单位（docs/specs/rust-cli/session-lifecycle.md §10.34）。活会话里它在 `ToolDone` 的 `output.display`；
/// 与这次调用的工具结果同批提交成 [`Fact::OutputHead`]，冷读由 [`crate::store::LoadedSession::output_heads`] 还原，会话的视图带上
/// （[`crate::session_api::SessionView::output_heads`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputHead {
    pub tool_use: ToolUseId,
    pub head: CommandOutputHead,
}

impl ToolOutputHead {
    /// 这次调用的结果里的头部；没有时为 None。同批落库的事实（[`CommandOutputHead::fact`]）与会话的视图（工具结束时记下，session-lifecycle.md §10.38）
    /// 都取它。
    pub fn of(tool_use: &ToolUseId, output: &ToolOutput) -> Option<Self> {
        CommandOutputHead::of(output).map(|head| Self {
            tool_use: tool_use.clone(),
            head,
        })
    }
}

#[cfg(test)]
#[path = "command_output_tests.rs"]
mod tests;
