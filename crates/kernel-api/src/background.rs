//! 会话里在跑的后台工作（K-W1，docs/specs/rust-cli/session-lifecycle.md §10.28）：开始的事实与工具结果里说明它的键。
//!
//! 后台工作是工具发起的：Bash 的后台命令与转后台的前台命令、后台子代理、续跑的子代理。发起它的工具把这件事写进结果的
//! `display`（只给界面，不落库，不进模型请求）；会话 actor 在主线程的工具结束时读它，记进自己的后台工作表，发
//! [`crate::session_api::KernelEvent::BackgroundStarted`]。monitor 由 actor 自己持有，开始在 `arm_monitor`。

use serde::{Deserialize, Serialize};

use crate::env::EnvPath;
use crate::env_shell::BackgroundStatus;
use crate::ids::{SessionId, ToolUseId};
use crate::manage::BackgroundKind;
use crate::tool::ToolOutput;

/// 工具结果的 `display` 里说明后台工作的键（与 [`crate::agents::DISPLAY_KEY`] 并列）。
pub const DISPLAY_KEY: &str = "background";

/// 一项后台工作开始时的事实（会话 actor 的后台工作表里的一项，`SessionView.works`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundStart {
    /// 任务 ID：命令是 shell 任务 ID，子代理是 agentId（同 [`crate::manage::BackgroundEntry::id`]，停止用它）。
    pub id: String,
    pub kind: BackgroundKind,
    /// 命令的说明（没有时是命令原文）、monitor 的说明、子代理的 description。
    pub description: String,
    /// 发起它的工具调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<ToolUseId>,
    /// 子代理的子会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<SessionId>,
    /// 会话 actor 记下它的时刻（Unix 毫秒，actor 的时钟）。
    pub started_at: u64,
}

/// 一项后台工作的终态通知开了一个回合（[`crate::session_api::TurnStart::works`]，docs/specs/rust-cli/session-lifecycle.md §10.50）：
/// 前端据此说明「这是哪项后台工作的结果」，不读通知的文字。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticedWork {
    /// 任务 ID（同 [`BackgroundStart::id`]）。
    pub id: String,
    pub kind: BackgroundKind,
    /// 命令的说明（没有时是命令原文）、子代理的 description；会话 actor 已经不记得这项工作时为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// 一次工具调用对后台工作做了什么（写在结果的 `display[DISPLAY_KEY]`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum BackgroundChange {
    /// 起了一项后台工作：后台命令、前台命令转到后台、后台子代理、前台子代理停放转后台、续跑一个已结束的子代理。
    Launched {
        id: String,
        kind: BackgroundKind,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<SessionId>,
    },
    /// 停了一条后台命令（TaskStop）：停了的命令没有终态通知，这一步就是它的结束。
    Stopped { id: String },
}

impl BackgroundChange {
    /// 工具结果里说明的变化；没有（或读不出）时为 None。
    pub fn of(output: &ToolOutput) -> Option<Self> {
        serde_json::from_value(output.display.get(DISPLAY_KEY)?.clone()).ok()
    }

    /// 写进工具结果的 `display`（保留别的键）。
    pub fn attach(self, mut output: ToolOutput) -> ToolOutput {
        output.display[DISPLAY_KEY] = serde_json::to_value(self).unwrap_or_default();
        output
    }
}

/// 一项后台命令（Shell、Monitor）现在的状态与输出文件的末尾（[`crate::session_api::SessionHandle::work_output`]，
/// docs/specs/rust-cli/session-lifecycle.md §10.44）。状态取会话 actor 的后台工作表：在跑的是 `Running`，结束了的是记下的终态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkOutput {
    pub status: BackgroundStatus,
    pub read: Result<OutputTail, OutputUnreadable>,
}

/// 输出文件的末尾。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTail {
    /// 输出文件（执行环境里的路径）。
    pub path: EnvPath,
    /// 最后至多 `tail_bytes` 字节（从字符边界开始）。
    pub text: String,
    /// 文件比 `text` 长。
    pub truncated: bool,
}

/// 读不到输出文件的末尾。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputUnreadable {
    /// 哪台机器上都没有这个文件（被删了）。
    NotFound,
    /// 别的读错误。
    Io,
}

impl OutputUnreadable {
    /// 线上的写法（前端照原样给出）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Io => "io",
        }
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
