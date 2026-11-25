//! rewind 的纯计算（docs/specs/rust-cli/session-lifecycle.md §5.1、§5.2）：可选消息、要撤销的
//! checkpoint 与文件恢复计划。不做 IO。回合的改动统计在 `turn_files`（§10.45）。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_rewind::{RestoreFile, content_hash};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::store::WorkspaceCheckpoint;
use serde::Deserialize;

/// 中断文本（合成的消息）；这两条不是可选的 user 消息（kernel-api 的定义，F55）。
const INTERRUPTS: [&str; 2] = rust_cli_kernel_api::user_input::INTERRUPT_TEXTS;
/// 可选消息排除的非用户内容标签。
const NOT_USER_TAGS: [&str; 7] = [
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<bash-stdout>",
    "<bash-stderr>",
    "<task-notification>",
    "<tick>",
    "<teammate-message",
];

fn last_text(user: &UserMessage) -> Option<&str> {
    match user.content.last()? {
        ContentBlock::Text { text } => Some(text.trim()),
        _ => None,
    }
}

/// 可选的消息：人类输入，不是工具结果 / meta / 压缩摘要 / 中断文本。
pub(crate) fn selectable(message: &Message) -> Option<&UserMessage> {
    let Message::User(user) = message else {
        return None;
    };
    if user.origin != UserOrigin::Human {
        return None;
    }
    let text = last_text(user).unwrap_or("");
    if INTERRUPTS.contains(&text) || NOT_USER_TAGS.iter().any(|tag| text.contains(tag)) {
        return None;
    }
    Some(user)
}

/// esc 撤回的判断（修复线 F38，spec §5.5）：输入之后的这些消息里有没有
/// 回答。有非空白文字或 tool_use 的 assistant、别的人类输入（含 `<bash-stdout>`）、压缩摘要算回答；中断标记、工具结果、
/// meta、attachment、system、只有 thinking 或空白文字的 assistant 不算。
pub(crate) fn answered(after: &[Message]) -> bool {
    after.iter().any(|message| match message {
        Message::Assistant(assistant) => assistant.content.iter().any(|block| match block {
            ContentBlock::Text { text } => !text.trim().is_empty(),
            ContentBlock::ToolUse { .. } => true,
            _ => false,
        }),
        Message::User(user) => match &user.origin {
            UserOrigin::Human => !INTERRUPTS.contains(&last_text(user).unwrap_or("")),
            UserOrigin::CompactSummary => true,
            UserOrigin::ToolResult { .. } | UserOrigin::Meta { .. } => false,
        },
        Message::Attachment(_) | Message::System(_) => false,
    })
}

/// target 起要撤销的 checkpoint：`prompt` 是活动历史里 target 或之后的消息（Node
/// `activeSuffixMessageIdsForRewind` + `selectCheckpointsForMessages`），按写入顺序。
pub(crate) fn checkpoints_from<'a>(
    messages: &[Message],
    checkpoints: &'a [WorkspaceCheckpoint],
    target: &MessageId,
) -> Vec<&'a WorkspaceCheckpoint> {
    let Some(index) = messages.iter().position(|m| m.id() == target) else {
        return Vec::new();
    };
    let suffix: Vec<&MessageId> = messages[index..].iter().map(Message::id).collect();
    checkpoints
        .iter()
        .filter(|c| suffix.contains(&&c.prompt) || suffix.contains(&&c.assistant))
        .collect()
}

/// checkpoint artifact 里本工程用到的字段（Node `workspaceCheckpointArtifactSchema`，`toolName` 是必有的）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Artifact {
    pub files: Vec<ArtifactFile>,
    /// 写这一次的工具（回合的文件改动的 `tools`，session-lifecycle.md §10.42；回退文件的预览也列出它，§10.44）。
    pub tool_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtifactFile {
    pub path: String,
    pub existed_before: bool,
    pub before_content: Option<String>,
    pub after_content: Option<String>,
}

/// 一个路径按写入的逆序模拟的结果（[`walk`]）：恢复计划与预览（spec §10.53）共用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Walked {
    pub path: String,
    /// 最后一次写入之后的内容的 hash：磁盘上现在应当是它（没有内容的 checkpoint 为 None）。
    pub expected: Option<String>,
    /// 恢复成的内容：最早一次写入之前的；那时不存在为 None。
    pub restore: Option<String>,
    /// 最早一次写入之前不存在：恢复就是删掉。
    pub delete: bool,
    pub writes: usize,
    /// 写它的工具，去重、排序。
    pub tools: Vec<String>,
    pub fault: Option<Fault>,
}

/// 一个路径模拟不下去的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fault {
    /// checkpoint 里没有写入之后的内容。
    NoContent,
    /// 相邻两次写入接不上（中间被别的方式改过）：较早那次写入之后的内容（期望）与较晚那次写入之前的内容（实际）的 hash。
    Gap {
        expected: Option<String>,
        current: Option<String>,
    },
}

/// 按写入的逆序、按路径模拟（Node `buildWorkspaceFileRewindPlan`）：每个路径期望磁盘上是最后一次写入后的内容，恢复成最早一次写入前的内容；
/// 中间两次写入接不上时这个路径出错、之后的写入只计数。返回路径（按第一次遇到的顺序）与按遍历顺序第一处错误的文字（[`plan`] 照它拒绝）。
pub(crate) fn walk(artifacts: &[Artifact]) -> (Vec<Walked>, Option<String>) {
    let mut walked: Vec<Walked> = Vec::new();
    let mut first_error = None;
    for artifact in artifacts.iter().rev() {
        for file in artifact.files.iter().rev() {
            let before = file.before_content.clone().filter(|_| file.existed_before);
            let index = match walked.iter().position(|w| w.path == file.path) {
                Some(index) => index,
                None => {
                    walked.push(Walked {
                        path: file.path.clone(),
                        expected: file
                            .after_content
                            .as_deref()
                            .map(|after| content_hash(after.as_bytes())),
                        restore: None,
                        delete: false,
                        writes: 0,
                        tools: Vec::new(),
                        fault: None,
                    });
                    walked.len() - 1
                }
            };
            let entry = &mut walked[index];
            entry.writes += 1;
            if !entry.tools.contains(&artifact.tool_name) {
                entry.tools.push(artifact.tool_name.clone());
                entry.tools.sort();
            }
            // 动作看最早的那次写入（逆序走到最后的那一次）
            entry.delete = before.is_none();
            if entry.fault.is_some() {
                continue;
            }
            let Some(after) = &file.after_content else {
                first_error.get_or_insert_with(|| {
                    format!("the checkpoint of {} has no content", file.path)
                });
                entry.fault = Some(Fault::NoContent);
                continue;
            };
            if entry.writes > 1 && entry.restore.as_deref() != Some(after.as_str()) {
                first_error.get_or_insert_with(|| conflict(&file.path));
                entry.fault = Some(Fault::Gap {
                    expected: Some(content_hash(after.as_bytes())),
                    current: entry
                        .restore
                        .as_deref()
                        .map(|text| content_hash(text.as_bytes())),
                });
                continue;
            }
            entry.restore = before;
        }
    }
    (walked, first_error)
}

/// 恢复计划（[`walk`]）：有一个路径模拟不下去就整体拒绝（按遍历顺序第一处的原因）。
pub(crate) fn plan(artifacts: &[Artifact]) -> Result<Vec<RestoreFile>, String> {
    let (walked, error) = walk(artifacts);
    if let Some(error) = error {
        return Err(error);
    }
    Ok(walked
        .into_iter()
        .map(|walked| RestoreFile {
            path: EnvPath::new(walked.path),
            expected: walked.expected,
            restore: walked.restore.map(String::into_bytes),
        })
        .collect())
}

/// 冲突的原因（每个文件一行）。
pub(crate) fn conflict(path: &str) -> String {
    format!("Cannot restore code: {path} was changed after ZCode edited it")
}

#[cfg(test)]
#[path = "rewind_plan_tests.rs"]
mod tests;
