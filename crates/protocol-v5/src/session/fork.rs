//! fork（`session.fork@1`，protocol-v5.md §55）：`turn.fork` 的目标是一行 assistantText，fork 到它所在的回合为止，新会话的 id 是参数里的
//! `sessionId`。
//!
//! ```text
//! turn.fork{sessionId, expect: assistantText 行} ─► 这一回合开头的输入 ─► ForkFromInputAs{input, session: sessionId}
//! ```
//!
//! 复制、准入（回合在跑时也分，只分已经结束的一轮）与「已经是这次分叉的会话」都是内核的（session-lifecycle.md §10.57）；这里只把行换成消息、
//! 把回执换成契约的形状。

use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::session_api::{Command, Forked};
use serde_json::{Value, json};

use super::commands::{Refused, live, payload, rejected, target, types};
use super::lane::Operation;
use super::rewind::{kind_mismatch, opening_input, row_kind};
use super::rows::{ROWS, kinds};
use crate::generated::contract::codes;
use crate::generated::core_v1::CommandEnvelope;
use crate::generated::session_fork_v1::{CommandResultTurnFork, CommandTurnFork};
use crate::publisher::Publisher;

/// `turn.fork`（§54.2）：回答行所在回合开头的输入（同 `turn.retry`），交给内核分到调用方给的 id。
pub(super) fn fork(
    envelope: &CommandEnvelope,
    publisher: Option<&Publisher>,
) -> Result<Operation, Refused> {
    let fork: CommandTurnFork = payload(types::TURN_FORK, &envelope.payload)?;
    let row = target(
        envelope.expect.as_ref(),
        ROWS,
        publisher,
        codes::PRECONDITION_NOT_FOUND,
    )?;
    let Some((kinds::ASSISTANT_TEXT, turn)) = row_kind(publisher, &row) else {
        return Err(kind_mismatch(kinds::ASSISTANT_TEXT));
    };
    let Some(input) = publisher.and_then(|p| opening_input(p, turn)) else {
        return Err(rejected(codes::PRECONDITION_NOT_FOUND, None, None));
    };
    Ok(live(Command::ForkFromInputAs {
        input: MessageId::new(input),
        session: SessionId::new(fork.session_id),
    }))
}

/// 分出的会话 → `CommandResult.turn.fork`（§54.2）：第一次与「已经是这次分叉的会话」同一个结果。
pub(super) fn result(forked: &Forked) -> Value {
    json!(CommandResultTurnFork {
        session_id: forked.session.as_str().to_owned(),
    })
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
