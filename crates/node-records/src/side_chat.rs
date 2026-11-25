//! 侧聊的新会话（docs/specs/rust-cli/session-lifecycle.md §10.58）：会话行、模型与模式、复制的消息（`fork_clone.rs`，只给模型看）与侧聊边界在调用方的
//! 一个事务里写完（本机 `begin immediate`，云端同 fork）。形状照 Node 的侧聊（`session-fork.ts` 的 `createSelectionSideConversation` → `commitAtomicConversationFork`），
//! Node 能读、能续写。与 fork 共用已有的检查、会话行与逐条克隆；不写目标状态、本工程的回合 / 输入 / 计划记录、时间线与分叉提示。
use std::borrow::Cow;

use rust_cli_kernel_api::store::side_chat::{BOUNDARY, SOURCE, TITLE};
use rust_cli_kernel_api::store::{Fact, ForkReceipt, SideChatBase, SideChatFork, StoreError};
use serde_json::json;

use crate::branch::{Branch, select_branch};
use crate::facts::{self, Commit};
use crate::fork_clone::{self, Copied};
use crate::ids::{message_id, part_id, turn_id};
use crate::row::WithParts;
use crate::rows::Rows;
use crate::{records, storage};

/// 侧聊会话的 `task_type`（Node `SESSION_TASK_TYPES` 的 `selection_side_chat`）。
pub const SIDE_CHAT_TASK_TYPE: &str = "selection_side_chat";
/// 只给模型看的消息的 `visibility`（Node `MessageVisibility`）。
const MODEL_ONLY: &str = "model-only";

/// 活动分支按 base 截到哪里（不含 `end`）。
fn end(branch: &[Cow<'_, WithParts>], base: &SideChatBase) -> Result<usize, StoreError> {
    let at = |id: &str| branch.iter().position(|record| record.id() == id);
    match base {
        SideChatBase::All => Ok(branch.len()),
        SideChatBase::Through(id) => at(id.as_str()).map(|index| index + 1).ok_or_else(|| {
            storage(format!(
                "Side chat base message not found in session store: {id}"
            ))
        }),
        // 回合的第一条消息被回退掉了：这一回合之前就是全部
        SideChatBase::Before(id) => Ok(at(id.as_str()).unwrap_or(branch.len())),
    }
}

/// 侧聊边界（Node `buildSelectionSideChatBoundary`）：只给模型看的合成 user 提醒，part 的 metadata 让 Node 与本工程都读成这个来源的提醒。
fn boundary(ctx: &Commit<'_>, fork: &SideChatFork) -> Result<(), StoreError> {
    let id = message_id(ctx.now, &(ctx.uuid)());
    let turn = turn_id(&(ctx.uuid)());
    let info = json!({"id": id, "sessionID": ctx.session, "role": "user",
        "time": {"created": ctx.now}, "agent": records::AGENT,
        "modelSelection": facts::node_selection(&fork.selection), "synthetic": true,
        "source": SOURCE, "visibility": MODEL_ONLY,
        "semantics": {"origin": "system", "kind": "system_reminder", "source": SOURCE,
            "uiVisibility": "hidden", "providerVisibility": "visible", "transcriptVisibility": "hidden"},
        "anchor": {"turnId": turn, "productTurnId": id, "orderedMessageIds": [id],
            "boundaryMessageId": id, "origin": "synthetic"}});
    ctx.conn.save_message(&info, ctx.now as i64)?;
    let part = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "text", "text": BOUNDARY, "synthetic": true,
        "time": {"start": ctx.now, "end": ctx.now},
        "metadata": {"runtimeMessage": {"source": SOURCE}, "source": SOURCE, "visibility": MODEL_ONLY}});
    ctx.conn.save_part(&part, ctx.now as i64)
}

/// 侧聊的新会话；调用方负责事务。
pub fn side_chat(
    tx: &dyn Rows,
    fork: &SideChatFork,
    (now, uuid): (u64, &dyn Fn() -> String),
) -> Result<ForkReceipt, StoreError> {
    if let Some(receipt) =
        crate::fork::existing(tx, (&fork.parent, &fork.child), SIDE_CHAT_TASK_TYPE)?
    {
        return Ok(receipt);
    }
    let parent = crate::fork::parent(tx, fork.parent.as_str())?;
    let input = crate::fork::child_row(
        &parent,
        (&fork.parent, &fork.child),
        (SIDE_CHAT_TASK_TYPE, TITLE.to_owned()),
        fork.created_at_ms,
    );
    let created = i64::try_from(fork.created_at_ms).unwrap_or(i64::MAX);
    tx.create_session(&input, created)?;
    let ctx = Commit::new(tx, fork.child.as_str(), (now, uuid), None, &[], None);
    facts::apply(
        &ctx,
        &Fact::ModelSelected {
            selection: fork.selection.clone(),
        },
    )?;
    let state = rust_cli_kernel_api::permission::ExecutionState::from_parts(fork.mode, fork.plan);
    facts::apply(&ctx, &Fact::ExecutionChanged { state })?;
    // 父会话的 fork 源分支：只应用 `session.revert`，不按压缩边界截断（同 fork）
    let records = tx.messages(fork.parent.as_str())?;
    let branch = select_branch(
        records.iter().map(Cow::Borrowed).collect(),
        &Branch::from_revert(parent.revert.as_ref()),
    );
    let end = end(&branch, &fork.base)?;
    let copied: Vec<&WithParts> = branch[..end].iter().map(AsRef::as_ref).collect();
    fork_clone::copy_messages(&ctx, &copied, Copied::ModelOnly)?;
    boundary(&ctx, fork)?;
    facts::next_sequence(&ctx)?;
    Ok(ForkReceipt::Created { copied: end })
}
