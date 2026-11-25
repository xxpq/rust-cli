//! 侧聊的新会话（[`SideChatFork`]，docs/specs/rust-cli/session-lifecycle.md §10.58）：从 store.rs 分出（800 行上限）。
//!
//! 侧聊是一种 fork：存储在一个事务里复制父会话到 [`SideChatBase`] 为止的历史，复制的消息只给模型看，末尾是侧聊边界（[`SOURCE`] 来源的
//! 合成 user 提醒，正文 [`BOUNDARY`]）。形状照 Node 的 `selection_side_chat`，Node 能读、能续写。

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, SessionId};
use crate::model::ModelSelection;
use crate::permission::PermissionMode;

/// 侧聊边界的来源（Node `SyntheticUserMessageSource` 的 `selection_side_chat`）：存储读成这个来源的 attachment，模型请求里按 user 角色发，
/// 前端的投影从它之后建行。
pub const SOURCE: &str = "selection_side_chat";

/// 侧聊边界的正文（Node `SELECTION_SIDE_CHAT_BOUNDARY`，三句以空格相连）：继承的历史只供参考，只回答侧聊里的新问题。
pub const BOUNDARY: &str = "The preceding conversation was inherited from the parent task for reference only. Do not continue the parent's active work automatically; answer only new questions sent in this side chat. Modify the workspace only when the user explicitly asks you to do so in this side chat.";

/// 侧聊会话的标题（Node `buildForkedSessionInput` 的 `selection_side_chat` 分支）：侧聊不进会话列表，标题只给存储与调试看。
pub const TITLE: &str = "Selection side chat";

/// 侧聊从父会话的活动分支带走到哪里：父会话最近一次稳定落盘的边界（会话 actor 按它此刻的回合定）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum SideChatBase {
    /// 没有回合在跑（或在跑的回合还没有消息落库）：全部。
    All,
    /// 在跑的回合由人类输入开：带到这条输入（含），不带之后落库的回答与工具结果。
    Through(MessageId),
    /// 在跑的回合不是人类输入开的（后台结果、定时任务）：带到它的第一条消息之前。
    Before(MessageId),
}

/// [`crate::store::SessionStore::side_chat`] 的参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SideChatFork {
    pub parent: SessionId,
    pub child: SessionId,
    pub created_at_ms: u64,
    pub base: SideChatBase,
    pub selection: ModelSelection,
    pub mode: PermissionMode,
    /// plan 开关（serde 缺省 false）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
}
