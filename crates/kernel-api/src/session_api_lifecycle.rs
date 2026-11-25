//! 会话生命周期的类型（docs/specs/rust-cli/session-lifecycle.md）；从 `session_api.rs` 拆出（400 行上限），
//! 经 `session_api` 重新导出，路径不变。

use serde::{Deserialize, Serialize};

use super::FileChange;
use crate::env::WorkspaceRef;
use crate::ids::{MessageId, SessionId};
use crate::model::ModelSelection;
use crate::permission::PermissionMode;

/// SessionStart hook 的 `source`（docs/specs/rust-cli/session-lifecycle.md §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartSource {
    Startup,
    Resume,
    Clear,
    Compact,
    Fork,
}

/// SessionEnd hook 的 `reason`（§3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Clear,
    Resume,
    Logout,
    PromptInputExit,
    Other,
}

/// rewind 选择器里的一条 user 消息（§5.1）：这一回合改过的文件与能否恢复代码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindOption {
    pub message: MessageId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileChange>,
    /// 这一回合起有可以恢复的 checkpoint。
    #[serde(default)]
    pub code: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSession {
    /// 第二阶段由 Host 给出会话 ID；为空时内核生成。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<SessionId>,
    pub workspace: WorkspaceRef,
    pub selection: ModelSelection,
    /// 权限模式；`Plan` 是旧写法（plan 开、default）。
    pub mode: PermissionMode,
    /// plan 开关（修复线 F20；serde 缺省 false）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    /// 什么时候落库（docs/specs/rust-cli/session-facts.md §4；serde 缺省立即）。
    #[serde(default)]
    pub persistence: Persistence,
    /// 建会话时带的附加工作目录（docs/specs/rust-cli/session-dirs.md §3.1）：接在内核缺省（`--add-dir`）之后，相对路径与已在
    /// 工作区根里的不收。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<crate::env::EnvPath>,
    /// 宿主给的会话偏好（docs/specs/rust-cli/app-prefs.md §4.2）：进会话的起始状态；重复的建会话回原会话，它不起作用。serde 缺省不关。
    #[serde(
        default,
        skip_serializing_if = "crate::session_preferences::SessionPreferences::is_default"
    )]
    pub preferences: crate::session_preferences::SessionPreferences,
}

/// 新会话什么时候写进存储（session-facts.md §4，02 §11 M2）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Persistence {
    /// 建会话时就写（TUI、`-p`）。
    #[default]
    Immediate,
    /// 草稿：会话行与之前攒下的事实随第一批带消息的提交落库；放弃的草稿不留痕。
    Deferred,
}
