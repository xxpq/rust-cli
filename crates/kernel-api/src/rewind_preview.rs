//! 回退文件的预览（docs/specs/rust-cli/session-lifecycle.md §10.53）：`Rewind{target, Code}` 会恢复哪些文件、现在能不能恢复。只读：
//! 不登记回退的 token、不写任何东西。前端在确认回退之前给用户看它，编辑带文件被挡住时也带上它。

use serde::{Deserialize, Serialize};

use crate::env_rewind::RestoreAction;
use crate::ids::MachineName;

/// 一次预览：要恢复的文件与读不出的 checkpoint。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindPreview {
    /// 按机器分组的顺序（主 env 在前，同回退的事务），组内按路径。
    pub files: Vec<PreviewFile>,
    /// artifact 读不出（不在、不是 Node 的形状）的 checkpoint：这些写入改了哪些文件不知道。
    pub unreadable: Vec<UnreadableCheckpoint>,
}

/// 一个要恢复的文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewFile {
    pub path: String,
    pub machine: MachineName,
    /// 恢复成最早一次写入之前的内容；最早一次写入之前它不存在时删掉。
    pub action: RestoreAction,
    /// 这些回合里写它的次数。
    pub writes: usize,
    /// 写它的工具（checkpoint 记下的工具名，去重、排序）。
    pub tools: Vec<String>,
    pub state: PreviewState,
}

/// 这个文件现在能不能恢复。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PreviewState {
    /// 磁盘上是最后一次写入之后的内容，中间相邻两次写入也接得上：能恢复。
    Ready,
    /// 磁盘上的内容（或中间某次写入之前的内容）不是期望的：在本工程写过之后又被别的方式改过。两个值都是
    /// [`crate::env_rewind::content_hash`]，None 表示文件不存在。
    Changed {
        expected: Option<String>,
        current: Option<String>,
    },
    /// checkpoint 里没有写入之后的内容（较早的形状）：核对不了。
    NoContent,
    /// 读不了现在的文件（读失败、远端机器没连上）。
    Unreadable { message: String },
}

/// 读不出的 checkpoint。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadableCheckpoint {
    pub checkpoint: String,
    pub message: String,
}
