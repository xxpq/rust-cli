//! 改写历史（`session.rewind@1`，protocol-v5.md §53）：`turn.edit`、`turn.retry`、`files.rewind` 三条命令与 `session.fileRewindPreview` 查询。
//!
//! ```text
//! turn.edit{expect: userInput 行}     ─► ReplaceInput{target: 行 id, 新输入（缺省沿用原输入的附件与引用）, restore_code: workspaceMode == rewind}
//! turn.retry{expect: assistantText 行} ─► 这一回合开头的输入 ─► ReplaceInput{target, 原输入的正文与记录}
//! files.rewind{expect: turnHeader 行}  ─► 这一回合开头的输入 ─► Rewind{target, Code}
//! session.fileRewindPreview{rowId}    ─► 同 queries.rs 的「答」：回合头在 Publisher 上判定 ─► 任务：Kernel::rewind_preview ─► 回复（actor 不等 IO）
//! ```
//!
//! 回退的语义是内核的（session-lifecycle.md §5、§10.53）；这里只把行换成消息、把内核的结果换成契约的形状。编辑与重试照 v4 让内核先停被截掉的
//! 回合里起的后台工作（`stop_cut_works`，§53.12、session-lifecycle.md §10.55）。

use rust_cli_kernel_api::env_rewind::RestoreAction;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::input_extras::{InputExtras, InputRecord};
use rust_cli_kernel_api::rewind_preview::{PreviewState, RewindPreview};
use rust_cli_kernel_api::session_api::{Ack, Command, Kernel, RewindScope};
use serde_json::{Value, json};

use super::commands::{Refused, live, payload, rejected, target, types};
use super::lane::{LiveOp, Operation};
use super::projection::Projection;
use super::rows::{self, CREATED_AT_SEQ, ROWS, keys, kinds};
use crate::errors::{Fault, protocol_error};
use crate::generated::contract::codes;
use crate::generated::core_v1::{CommandEnvelope, Cursor};
use crate::generated::session_rewind_v1::{
    CommandResultTurnEdit, CommandResultTurnEditDisposition, CommandTurnEdit,
    CommandTurnEditWorkspaceMode, QuerySessionFileRewindPreviewResult,
    QuerySessionFileRewindPreviewResultSafeFilesItem,
    QuerySessionFileRewindPreviewResultSafeFilesItemAction,
    QuerySessionFileRewindPreviewResultUnsafeFilesItem,
    QuerySessionFileRewindPreviewResultUnsafeFilesItemReason,
};
use crate::publisher::Publisher;

/// 回退文件的预览查询（`session.rewind@1` 的 `SESSION_QUERIES`）。
pub(crate) const FILE_REWIND_PREVIEW: &str = "session.fileRewindPreview";

/// `turn.edit` 与 `input.send` 不同名的两个字段（新输入的正文；只有编辑有的文件模式）：其余字段同形（02 §6：编辑补齐的字段与 input.send 一致）。
pub(super) const NEW_TEXT: &str = "newText";
const WORKSPACE_MODE: &str = "workspaceMode";
/// 读不出的 checkpoint 在预览里的路径前缀（同 Node `file-rewind.ts`）。
const CHECKPOINT_PATH_PREFIX: &str = "checkpoint:";
/// 远端机器上的文件在预览里写成 `<机器名>:<路径>`（同 `/rewind` 选择器给远端文件带机器名）。
const MACHINE_PATH_SEPARATOR: &str = ":";
/// 文件不存在时的 `currentHash`（同 Node）。
const MISSING_HASH: &str = "missing";
/// 读不出的 checkpoint 算一次写入。
const UNREADABLE_WRITES: u64 = 1;

/// 目标行的种类不对（02 §4.2 的行级前提）。
pub(super) fn kind_mismatch(expected: &str) -> Refused {
    rejected(
        codes::ADMISSION_ROW_KIND_MISMATCH,
        None,
        Some(format!("the target row is not a {expected} row")),
    )
}

/// 一行的种类与回合（行不在时为 None）。
pub(super) fn row_kind<'a>(
    publisher: Option<&'a Publisher>,
    id: &str,
) -> Option<(&'a str, &'a str)> {
    // 种类与回合读骨架也够（窗口外的行可能只剩骨架，§67.0 第 5 条）
    let publisher = publisher?;
    publisher.version(ROWS, id)?;
    let text = |key: &str| {
        publisher
            .field(ROWS, id, key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    Some((text(keys::KIND), text(keys::TURN_ID)))
}

/// 这一回合开头的输入：同一回合里 `createdAtSeq` 最小、不是插话的 userInput 行（它就是开回合的那条人类输入，内核先提交输入再提交它的
/// attachment）。通知、定时任务开的回合没有。
pub(super) fn opening_input(publisher: &Publisher, turn: &str) -> Option<String> {
    let page = publisher.page(ROWS, CREATED_AT_SEQ, None, usize::MAX)?;
    page.items.into_iter().find_map(|row| {
        let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
        (text(keys::KIND) == kinds::USER_INPUT
            && text(keys::TURN_ID) == turn
            && row.get(keys::GUIDED).and_then(Value::as_bool) != Some(true))
        .then(|| text(crate::publisher::ID_KEY).to_owned())
    })
}

/// 输入记录 → 重发这条输入时的事实：附件、上传、引用与来源（02 §11.7 审定第 3 项：重试保留来源）。页面状态、禁用名单、配置不在记录里。
fn recorded(record: Option<&InputRecord>) -> InputExtras {
    let Some(record) = record else {
        return InputExtras::default();
    };
    InputExtras {
        files: record.files.clone(),
        uploads: record.uploads.clone(),
        quotes: record.quotes.clone(),
        relay: record.relay.clone(),
        ..InputExtras::default()
    }
}

/// `turn.edit`（§53.2）：新输入按 `input.send` 的同一组换法；`attachments`、`quotes` 没给时沿用原输入的（02 §6、§11.6），编辑不带来源。
pub(super) fn edit(
    envelope: &CommandEnvelope,
    projection: Option<&Projection>,
    publisher: Option<&Publisher>,
) -> Result<Operation, Refused> {
    let edit: CommandTurnEdit = payload(types::TURN_EDIT, &envelope.payload)?;
    let row = target(
        envelope.expect.as_ref(),
        ROWS,
        publisher,
        codes::PRECONDITION_NOT_FOUND,
    )?;
    if row_kind(publisher, &row).map(|(kind, _)| kind) != Some(kinds::USER_INPUT) {
        return Err(kind_mismatch(kinds::USER_INPUT));
    }
    let send = super::inputs::as_send(
        &envelope.payload,
        &[NEW_TEXT, WORKSPACE_MODE],
        &edit.new_text,
    )?;
    let mut extras = super::inputs::extras(&send)?;
    let target = MessageId::new(&row);
    let original = recorded(projection.and_then(|p| p.record(&target)));
    if edit.attachments.is_none() {
        extras.files = original.files;
        extras.uploads = original.uploads;
    }
    if edit.quotes.is_none() {
        extras.quotes = original.quotes;
    }
    if send.text.trim().is_empty() && !extras.gives_the_model_something() {
        return Err(rejected(codes::PRECONDITION_INPUT_EMPTY, None, None));
    }
    let command = Command::ReplaceInput {
        target: target.clone(),
        text: send.text,
        extras,
        restore_code: edit.workspace_mode == Some(CommandTurnEditWorkspaceMode::Rewind),
        // 照 v4：先停被截掉的回合里起的后台工作，它们迟到的通知不进新分支（§53.12，内核按自己的记录在截断的同一步做）
        stop_cut_works: true,
    };
    Ok(Operation::Live(LiveOp::Edit { command, target }))
}

/// `turn.retry`（§53.2）：assistantText 行所在回合开头的输入，按它的正文与记录重跑。
pub(super) fn retry(
    envelope: &CommandEnvelope,
    projection: Option<&Projection>,
    publisher: Option<&Publisher>,
) -> Result<Operation, Refused> {
    payload::<Value>(types::TURN_RETRY, &envelope.payload)?;
    let row = target(
        envelope.expect.as_ref(),
        ROWS,
        publisher,
        codes::PRECONDITION_NOT_FOUND,
    )?;
    let Some((kinds::ASSISTANT_TEXT, turn)) = row_kind(publisher, &row) else {
        return Err(kind_mismatch(kinds::ASSISTANT_TEXT));
    };
    let Some((publisher, input)) = publisher.and_then(|p| Some((p, opening_input(p, turn)?)))
    else {
        return Err(rejected(codes::PRECONDITION_NOT_FOUND, None, None));
    };
    let text = publisher
        .entity(ROWS, &input)
        .map(|fields| rows::text(fields, keys::TEXT).to_owned())
        .unwrap_or_default();
    let target = MessageId::new(&input);
    let extras = recorded(projection.and_then(|p| p.record(&target)));
    Ok(live(Command::ReplaceInput {
        target,
        text,
        extras,
        restore_code: false,
        stop_cut_works: true,
    }))
}

/// `files.rewind`（§53.2）：回合头所在回合开头的输入，回退它起改过的文件（内核的 `Rewind{Code}`：回到这条输入之前）。
pub(super) fn files(
    envelope: &CommandEnvelope,
    publisher: Option<&Publisher>,
) -> Result<Operation, Refused> {
    payload::<Value>(types::FILES_REWIND, &envelope.payload)?;
    let row = target(
        envelope.expect.as_ref(),
        ROWS,
        publisher,
        codes::PRECONDITION_NOT_FOUND,
    )?;
    if row_kind(publisher, &row).map(|(kind, _)| kind) != Some(kinds::TURN_HEADER) {
        return Err(kind_mismatch(kinds::TURN_HEADER));
    }
    let Some(input) = publisher.and_then(|p| opening_input(p, &row)) else {
        return Err(rejected(codes::PRECONDITION_NOT_FOUND, None, None));
    };
    Ok(live(Command::Rewind {
        target: MessageId::new(input),
        scope: RewindScope::Code,
    }))
}

/// 内核说文件恢复不了（编辑带文件时，§10.53 的 `Unavailable`）：结果是 `blocked`，不是拒绝（契约的 `CommandResult.turn.edit`）。重发拿到的
/// 原结果同样算。
pub(super) fn blocked(ack: &Ack) -> bool {
    match ack {
        Ack::Rejected {
            refusal: rust_cli_kernel_api::admission::Refusal::Unavailable,
            ..
        } => true,
        Ack::Duplicate { original } => blocked(original),
        Ack::Accepted { .. } | Ack::Rejected { .. } | Ack::OutcomeUnknown { .. } => false,
    }
}

/// `turn.edit` 改写了。
pub(super) fn rewound_result() -> Value {
    json!(CommandResultTurnEdit {
        disposition: CommandResultTurnEditDisposition::Rewind,
        reason_code: None,
        preview: None,
    })
}

/// `turn.edit` 被挡住：文件恢复不了（§53.2），带那时的预览（冲突框据它显示；会话在回执之后关了时没有）。
pub(super) fn blocked_result(preview: Option<Value>) -> Value {
    json!(CommandResultTurnEdit {
        disposition: CommandResultTurnEditDisposition::Blocked,
        reason_code: Some(codes::PRECONDITION_REWIND_UNAVAILABLE.to_owned()),
        preview,
    })
}

/// 内核的预览 → 契约的形状（§53.3 的表）。`canApply` 同 Node：有安全的文件、没有不安全的；Bash 改的文件 rust 不记，`ignoredFiles` 恒空。
pub(crate) fn preview_result(preview: &RewindPreview) -> Value {
    let mut safe = Vec::new();
    let mut unsafe_files = Vec::new();
    for file in &preview.files {
        let path = match file.machine.remote() {
            None => file.path.clone(),
            Some(name) => format!("{name}{MACHINE_PATH_SEPARATOR}{}", file.path),
        };
        let operation_count = u64::try_from(file.writes).unwrap_or(u64::MAX);
        let unsafe_item = |reason, expected_hash, current_hash, message| {
            QuerySessionFileRewindPreviewResultUnsafeFilesItem {
                current_hash,
                expected_hash,
                message,
                operation_count,
                path: path.clone(),
                reason,
                tool_names: file.tools.clone(),
            }
        };
        match &file.state {
            PreviewState::Ready => safe.push(QuerySessionFileRewindPreviewResultSafeFilesItem {
                action: match file.action {
                    RestoreAction::Restore => {
                        QuerySessionFileRewindPreviewResultSafeFilesItemAction::Restore
                    }
                    RestoreAction::Delete => {
                        QuerySessionFileRewindPreviewResultSafeFilesItemAction::Delete
                    }
                },
                operation_count,
                path: path.clone(),
                tool_names: file.tools.clone(),
            }),
            PreviewState::Changed { expected, current } => unsafe_files.push(unsafe_item(
                QuerySessionFileRewindPreviewResultUnsafeFilesItemReason::ExternalModified,
                expected.clone(),
                Some(current.clone().unwrap_or_else(|| MISSING_HASH.to_owned())),
                None,
            )),
            PreviewState::NoContent => unsafe_files.push(unsafe_item(
                QuerySessionFileRewindPreviewResultUnsafeFilesItemReason::UnsupportedCheckpoint,
                None,
                None,
                None,
            )),
            PreviewState::Unreadable { message } => unsafe_files.push(unsafe_item(
                QuerySessionFileRewindPreviewResultUnsafeFilesItemReason::FileReadFailed,
                None,
                None,
                Some(message.clone()),
            )),
        }
    }
    unsafe_files.extend(preview.unreadable.iter().map(|checkpoint| {
        QuerySessionFileRewindPreviewResultUnsafeFilesItem {
            current_hash: None,
            expected_hash: None,
            message: Some(checkpoint.message.clone()),
            operation_count: UNREADABLE_WRITES,
            path: format!("{CHECKPOINT_PATH_PREFIX}{}", checkpoint.checkpoint),
            reason: QuerySessionFileRewindPreviewResultUnsafeFilesItemReason::CheckpointUnreadable,
            tool_names: Vec::new(),
        }
    }));
    safe.sort_by(|a, b| a.path.cmp(&b.path));
    unsafe_files.sort_by(|a, b| a.path.cmp(&b.path));
    json!(QuerySessionFileRewindPreviewResult {
        can_apply: !safe.is_empty() && unsafe_files.is_empty(),
        ignored_files: Vec::new(),
        safe_files: safe,
        unsafe_files,
    })
}

/// 回退文件的预览（§53.3）：判定在 Publisher 上做完（queries.rs），这里经内核读、换成契约的形状。目标是 None（这一回合没有开头的输入）或不是可选
/// 的输入：空的预览。
pub(super) async fn preview(
    kernel: &dyn Kernel,
    sid: &str,
    target: Option<MessageId>,
    cursor: Cursor,
) -> Result<Value, Fault> {
    let preview = match target {
        Some(target) => kernel
            .rewind_preview(&SessionId::new(sid), &target)
            .await
            .map_err(|error| {
                // 同文件改动的明细（queries.rs 的 `file_diffs`）：会话不在 → notFound，别的是内部错误
                let rejection = super::lane::not_found(error);
                Box::new(protocol_error(rejection.code, rejection.message))
            })?
            .unwrap_or_default(),
        None => RewindPreview::default(),
    };
    Ok(super::queries::ok(preview_result(&preview), cursor))
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
