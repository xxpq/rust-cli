//! `MemoryStore` 的回放（从 store.rs 分出，800 行上限）：提交过的事实 → 活动历史、checkpoint、标题、首条输入、回合记录。

use std::collections::HashSet;

use crate::ids::MessageId;
use crate::message::Message;
use crate::store::{Fact, WorkspaceCheckpoint};

/// 重放事实得到活动历史与 checkpoint 清单：`RevertSet` 之后活动历史 = `kept` + 切点之后追加的
/// 消息（docs/specs/rust-cli/session-lifecycle.md §5.3）；`CheckpointsRestored` 把撤销过的挪进第三项（§10.54）。
pub(super) fn replay(
    facts: &[Fact],
) -> (
    Vec<Message>,
    Vec<WorkspaceCheckpoint>,
    Vec<WorkspaceCheckpoint>,
) {
    let mut all: Vec<Message> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let mut checkpoints: Vec<WorkspaceCheckpoint> = Vec::new();
    let mut reverted = Vec::new();
    for fact in facts {
        match fact {
            Fact::MessageAppended { message } => {
                all.push(message.clone());
                active.push(all.len() - 1);
            }
            Fact::RevertSet { cursor } => {
                active = cursor
                    .kept
                    .iter()
                    .filter_map(|id| all.iter().position(|m| m.id() == id))
                    .collect();
            }
            Fact::CheckpointCreated { checkpoint } => checkpoints.push(checkpoint.clone()),
            Fact::CheckpointsRestored { checkpoints: gone } => {
                let (undone, kept) = checkpoints.into_iter().partition(|c| gone.contains(&c.id));
                checkpoints = kept;
                reverted.extend::<Vec<_>>(undone);
            }
            _ => {}
        }
    }
    (
        active.into_iter().map(|i| all[i].clone()).collect(),
        checkpoints,
        reverted,
    )
}

/// 冷读取按 node-store 的形状切分（工作线 F102，docs/specs/rust-cli/compact.md §5.1）：活动历史（最后一个压缩点起，
/// 保留段接在摘要之后）是模型侧的历史；之前的、不在活动历史里、也不是保留段首尾的消息只供显示。返回（只供显示，模型侧）。
pub(super) fn split_cold(messages: Vec<Message>) -> (Vec<Message>, Vec<Message>) {
    let Some((start, boundary)) = crate::history::last_boundary(&messages) else {
        return (Vec::new(), messages);
    };
    let mut skip: HashSet<MessageId> = HashSet::new();
    if let Some(segment) = &boundary.preserved {
        skip.extend([segment.head.clone(), segment.tail.clone()]);
    }
    let active: Vec<Message> = crate::history::active(&messages)
        .into_iter()
        .map(std::borrow::Cow::into_owned)
        .collect();
    skip.extend(active.iter().map(|message| message.id().clone()));
    let earlier = messages[..start]
        .iter()
        .filter(|message| !skip.contains(message.id()))
        .cloned()
        .collect();
    (earlier, active)
}

/// 一种 `Fact::Entry` 按第一次写入的顺序：`(id, data)`（`SessionStore::entries` 与冷读的回合记录共用）。同一个 id 再写是更新，
/// 取最后写的 data、位置不变（同 node-store 的 upsert：回合记录在回合结束时再写一次，session-lifecycle.md §10.17）。
pub(super) fn entries(facts: &[Fact], kind: &str) -> Vec<(String, serde_json::Value)> {
    let mut out: Vec<(String, serde_json::Value)> = Vec::new();
    for fact in facts {
        let Fact::Entry {
            kind: entry,
            id,
            data,
        } = fact
        else {
            continue;
        };
        if entry != kind {
            continue;
        }
        match out.iter_mut().find(|(seen, _)| seen == id) {
            Some((_, stored)) => stored.clone_from(data),
            None => out.push((id.clone(), data.clone())),
        }
    }
    out
}

/// 最后一条开启新一轮的输入之后，最后一次回合结束是错误（node-store 按 `turn_usage` 的同一口径）。
pub(super) fn last_turn_failed(facts: &[Fact]) -> bool {
    let mut failed = false;
    for fact in facts {
        match fact {
            Fact::MessageAppended {
                message: Message::User(user),
            } if crate::user_input::opens_turn(user) => failed = false,
            Fact::TurnEnded { status } => {
                failed = *status == crate::metering::CallStatus::Error;
            }
            _ => {}
        }
    }
    failed
}

/// 最后一次写入的标题（用户改名与生成的标题）。
pub(super) fn last_title(facts: &[Fact]) -> Option<String> {
    facts.iter().rev().find_map(|fact| match fact {
        Fact::TitleChanged { title } | Fact::TitleGenerated { title, .. } => Some(title.clone()),
        _ => None,
    })
}

/// 第一条人类输入的文字（node-store 取首条真实用户输入的第一个文字 part）。
pub(super) fn first_prompt(facts: &[Fact]) -> Option<String> {
    facts.iter().find_map(|fact| match fact {
        Fact::MessageAppended {
            message: Message::User(user),
        } if user.origin == crate::message::UserOrigin::Human => {
            user.content.iter().find_map(|block| match block {
                crate::message::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    })
}
