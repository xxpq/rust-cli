//! 会话 actor 的命令账本（docs/specs/rust-cli/session-lifecycle.md §10.3，K-A1）：命令 id → 命令摘要与原结果。
//!
//! 以前是 `seen: HashSet<CommandId>`：重复的命令回一个不带内容的 `Duplicate`（重发的一方不知道原来的结果），同 id、不同内容的
//! 命令也当重复吞掉。账本在 actor 的寿命内全量保留；打开时从效果记录（`LoadedSession.sources` 里的命令，§10.33）收回的 id 没有
//! 摘要，原结果按 `Accepted` 重建（§10.3）。

use std::collections::HashMap;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::session_api::{Ack, CommandEnvelope};
use tokio::sync::oneshot;

/// 同一个 id 已经用于另一条命令（`Rejected.reason`，给前端看）。
pub(crate) const ID_CONFLICT: &str = "the command id was already used for another command";

/// 一条记下的命令。
struct Entry {
    /// 命令的摘要；从效果记录收回的没有（跨进程只按 id 判，§10.3）。
    digest: Option<String>,
    result: Recorded,
}

enum Recorded {
    Done(Ack),
    /// 还在等结果（回退、起名）：之后到达的重发等它，同一个结果回给它们。
    Pending(Vec<oneshot::Sender<Ack>>),
}

/// 还没回执的一条新命令：结果出来时交给 [`CommandLedger::settle`]。
pub(crate) struct Reply {
    id: CommandId,
    sender: oneshot::Sender<Ack>,
}

#[derive(Default)]
pub(crate) struct CommandLedger {
    entries: HashMap<CommandId, Entry>,
}

/// 命令的摘要：`Command` 的 JSON 的 sha256。
fn digest(envelope: &CommandEnvelope) -> String {
    // `Command` 只有字符串、数字与枚举，序列化不会失败。
    let bytes = serde_json::to_vec(&envelope.command).unwrap_or_default();
    rust_cli_base::hash::sha256_hex(&bytes)
}

impl CommandLedger {
    /// 打开时的账本：效果记录里的 id（§10.3）。
    pub(crate) fn seeded(ids: impl IntoIterator<Item = CommandId>) -> Self {
        let done = || Recorded::Done(crate::actor::accepted());
        Self {
            entries: ids
                .into_iter()
                .map(|id| {
                    (
                        id,
                        Entry {
                            digest: None,
                            result: done(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// 查账：认识这个 id 时在这里回执（重复回原结果、冲突回拒绝、原命令还在等时排队），返回 None；新命令记成等结果，
    /// 返回它的 [`Reply`]。
    pub(crate) fn admit(
        &mut self,
        envelope: &CommandEnvelope,
        sender: oneshot::Sender<Ack>,
    ) -> Option<Reply> {
        let digest = digest(envelope);
        let Some(entry) = self.entries.get_mut(&envelope.id) else {
            self.entries.insert(
                envelope.id.clone(),
                Entry {
                    digest: Some(digest),
                    result: Recorded::Pending(Vec::new()),
                },
            );
            return Some(Reply {
                id: envelope.id.clone(),
                sender,
            });
        };
        if entry.digest.as_ref().is_some_and(|known| *known != digest) {
            let _ = sender.send(crate::actor::rejected(
                Refusal::CommandIdConflict,
                ID_CONFLICT,
            ));
            return None;
        }
        match &mut entry.result {
            Recorded::Done(original) => {
                let _ = sender.send(Ack::Duplicate {
                    original: Box::new(original.clone()),
                });
            }
            Recorded::Pending(waiting) => waiting.push(sender),
        }
        None
    }

    /// 结果出来了：记下，回给这条命令与等着它的重发。结果本身是 `Duplicate`（持久的输入账本认出别的进程执行过它，
    /// session 的 durable_inputs.rs）时记下它包着的原结果：之后的重发回 `Duplicate { original: 原结果 }`，不一层套一层。
    pub(crate) fn settle(&mut self, reply: Reply, ack: Ack) {
        let original = match &ack {
            Ack::Duplicate { original } => original.as_ref().clone(),
            other => other.clone(),
        };
        if let Some(entry) = self.entries.get_mut(&reply.id) {
            let previous = std::mem::replace(&mut entry.result, Recorded::Done(original.clone()));
            if let Recorded::Pending(waiting) = previous {
                for sender in waiting {
                    let _ = sender.send(Ack::Duplicate {
                        original: Box::new(original.clone()),
                    });
                }
            }
        }
        let _ = reply.sender.send(ack);
    }
}
