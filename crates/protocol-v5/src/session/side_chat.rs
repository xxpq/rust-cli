//! 侧聊（protocol-v5.md §57）：`session.create{kind: sideChat}` 在父会话的通道里执行内核的 `SideChatAs`，新会话的 id 是参数里的
//! `sessionId`；带首条输入时，父会话分出侧聊之后连接的 actor 把它交给侧聊的通道（同一个 commandId），ACK 等它的结局。
//!
//! ```text
//! session.create{kind: sideChat, parent, sessionId, firstInput?}
//!   ─► 父会话的通道：SideChatAs{sessionId}（复制、准入、「已有」都是内核的，session-lifecycle.md §10.58）
//!   ─► Forked{session} ─┬─ 没有首条输入 ─► accepted{result{sessionId}}
//!                       └─ 有 ─► 侧聊的通道：input.send 的同一个内核命令 ─► accepted{result{sessionId}}
//! ```
//!
//! 侧聊会话的门（§57.4）在这里列，通道按会话行的种类拒绝；投影按 `meta.kind` 从侧聊边界之后建行（§57.5）。

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{Ack, Command, CommandResult as KernelResult, SessionView};
use rust_cli_kernel_api::store::SessionKind;
use serde_json::{Value, json};

use super::commands::{
    Accepted, Refused, input, invalid, payload, refusal_result, rejected, types,
};
use super::lane::{Operation, Rejection};
use crate::generated::contract::codes;
use crate::generated::core_v1::{CommandResult, CommandResultDuplicate};
use crate::generated::session_v1::{CommandInputSend, CommandSessionCreate};

/// 侧聊会话上拒绝的命令（02 §4.2 的禁止表，按目标会话；§57.4）。goal、workflow 的命令 rust 不声明能力，在能力上先拦下，不列在这里。
/// 压缩照 v4 允许（§57.11 审定第 1 项）。
pub(super) const FORBIDDEN: [&str; 6] = [
    types::TURN_FORK,
    types::TURN_EDIT,
    types::TURN_RETRY,
    types::FILES_REWIND,
    types::SESSION_RENAME,
    types::SESSION_DELETE,
];

/// 同一组命令在 `state.admission` 里的门（§57.4）：侧聊会话上写成 `deny(admission.sideChat)`。
pub(super) const GATES: [&str; 4] = ["fork", "edit", "retry", "rewindFiles"];

/// 首条输入的参数名（拒绝时 `field` 的前缀）。
const FIRST_INPUT: &str = "firstInput";

/// 这一步在侧聊会话上不做：禁止表里的命令，或以它为父会话再开侧聊。
pub(super) fn forbidden(kind: &str, op: &Operation) -> bool {
    matches!(op, Operation::SideChat { .. }) || FORBIDDEN.contains(&kind)
}

/// 侧聊会话上的拒绝（通道按会话事实的种类给出）。
pub(super) fn refused(kind: &str) -> Rejection {
    Rejection {
        code: codes::ADMISSION_SIDE_CHAT,
        message: format!("a side chat does not take {kind}"),
    }
}

/// `session.create{kind: sideChat}`（§57.2）：要 `parent`；历史来自父会话，不接受 `importedHistory`；`firstInput` 按 `input.send` 的 schema 与换算读；
/// 其余创建期选项接受后丢弃（侧聊的一切从父会话来，v4 同）。命令落在父会话的通道里。
pub(super) fn accept(key: String, create: CommandSessionCreate) -> Result<Accepted, Refused> {
    let Some(parent) = create.parent.filter(|parent| !parent.is_empty()) else {
        return Err(invalid("parent", "a side chat names its parent session"));
    };
    if create.imported_history.is_some() {
        return Err(rejected(
            codes::PRECONDITION_CAPABILITY_UNSUPPORTED,
            None,
            Some("a side chat takes its history from the parent session".to_owned()),
        ));
    }
    let first = create.first_input.as_ref().map(first_input).transpose()?;
    Ok(Accepted {
        key,
        sid: parent,
        op: Operation::SideChat {
            command: Command::SideChatAs {
                session: SessionId::new(create.session_id),
            },
            first,
        },
    })
}

/// 首条输入：一条 `input.send` 的参数（同一份 schema、同一个换算，§21）。拒绝的字段路径前加 `firstInput`。
fn first_input(value: &Value) -> Result<Command, Refused> {
    let send: CommandInputSend = payload(types::INPUT_SEND, value).map_err(|mut refused| {
        if let CommandResult::Rejected(rejected) = refused.as_mut() {
            rejected.field = Some(match rejected.field.take().filter(|f| !f.is_empty()) {
                Some(field) => format!("{FIRST_INPUT}.{field}"),
                None => FIRST_INPUT.to_owned(),
            });
        }
        refused
    })?;
    input(send)
}

/// 父会话分出了侧聊（不是重放）：首条输入接着交给它。重放（`Duplicate`）时第一次已经带着首条输入，不再发。
pub(super) fn created(ack: &Ack) -> Option<SessionId> {
    match ack {
        Ack::Accepted {
            result: KernelResult::Forked(forked),
        } => Some(forked.session.clone()),
        _ => None,
    }
}

/// `session.create` 的结局里的内核回执 → `CommandResult.session.create`（§57.2）。`sid` 是这一步所在的会话：父会话（分出的会话在回执里）
/// 或侧聊（首条输入的回执，结果仍是侧聊的 id）。
pub(super) fn done(
    sid: &str,
    ack: Ack,
    accepted: &dyn Fn(bool, Option<Value>) -> CommandResult,
) -> Result<CommandResult, Rejection> {
    match ack {
        Ack::Accepted { result } => {
            let session = match result {
                KernelResult::Forked(forked) => forked.session.as_str().to_owned(),
                _ => sid.to_owned(),
            };
            Ok(accepted(false, Some(json!({"sessionId": session}))))
        }
        // 重放（02 §6）：原来的回执按同一张表换算
        Ack::Duplicate { original } => {
            let original = done(sid, *original, accepted)?;
            Ok(CommandResult::Duplicate(CommandResultDuplicate {
                original: serde_json::to_value(original).unwrap_or(Value::Null),
            }))
        }
        Ack::Rejected { refusal, reason } => {
            refusal_result(types::SESSION_CREATE, refusal, reason, None)
        }
        Ack::OutcomeUnknown { reason } => Ok(super::commands::outcome_unknown(reason)),
    }
}

/// 成行的消息（§57.5）：侧聊会话从最后一条侧聊边界之后开始，之前是继承的历史（只给模型看）；别的会话是全部。
/// 侧聊的起点（父会话历史之后的那条侧聊附件）在这个视图里（§73.2）：视图只是最后一段时，有它就不必再往前读，它之前的不显示。
pub(super) fn starts_in(view: &SessionView) -> bool {
    view.meta.kind == SessionKind::SideChat && shown(view).len() < view.messages.len()
}

pub(super) fn shown(view: &SessionView) -> &[Message] {
    if view.meta.kind != SessionKind::SideChat {
        return &view.messages;
    }
    let start = view
        .messages
        .iter()
        .rposition(|message| {
            matches!(message, Message::Attachment(attachment)
                if attachment.source == rust_cli_kernel_api::store::side_chat::SOURCE)
        })
        .map_or(0, |boundary| boundary + 1);
    &view.messages[start..]
}

#[cfg(test)]
#[path = "side_chat_tests.rs"]
mod tests;
