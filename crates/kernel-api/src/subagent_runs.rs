//! 会话里主线程派生的子代理（docs/specs/rust-cli/session-lifecycle.md §10.56）：Agent 工具交给会话的开始，与会话 actor 的子代理表里的一项。
//!
//! 前端无关：内核只说「哪个 Agent 调用派生了哪个子代理、它在前台还是后台、怎么结束」。会话 actor 是表的唯一 owner：开始经
//! [`crate::session_ops::SessionOps::subagent_started`]，转后台与前台的结束看这次调用的工具结束，后台的结束看终态通知；每次变化发
//! [`crate::session_api::KernelEvent::SubagentRecorded`]，视图的 `subagents` 是表里的全部。

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, SessionId, ToolUseId};

/// Agent 工具派生了一个子代理：子会话与它的第一条输入已落库，子代理还没有跑。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentStart {
    /// 派生它的 Agent 调用。
    pub tool_use: ToolUseId,
    pub agent: AgentId,
    pub child: SessionId,
    /// agent 定义的类型（`general-purpose`、`Explore`、自定义 agent 的名字、`fork`）。
    pub agent_type: String,
    /// 模型给的说明（Agent 的 `description`）。
    pub description: String,
    /// 在后台：派生时就在后台，或停放、续跑之后（会话 actor 改它）。
    pub background: bool,
}

/// 子代理怎么结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentOutcome {
    Completed,
    Failed,
    /// 被停（父回合被停、`StopTask` / TaskStop 停了后台子代理）。
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentEnded {
    pub outcome: SubagentOutcome,
    /// 会话 actor 记下结束的时刻（Unix 毫秒）。
    pub at: u64,
}

/// 会话 actor 的子代理表里的一项（`SessionView.subagents`、`KernelEvent::SubagentRecorded`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRun {
    #[serde(flatten)]
    pub start: SubagentStart,
    /// 会话 actor 记下开始的时刻（Unix 毫秒）；续跑时换成续跑的那一刻。
    pub started_at: u64,
    /// 在跑时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<SubagentEnded>,
}
