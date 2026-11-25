//! `MemoryStore` 的侧聊（session-lifecycle.md §10.58 的内存替身，从 store.rs 分出，800 行上限）：活动历史按 base 截、原样复制（内存里不必换 ID，
//! 也不分「只给模型看」：替身的冷读不分显示与模型），末尾是一条侧聊边界的 attachment。前端按边界藏起继承的历史，与 node-store 同一个读法。

use rust_cli_base::sync::LockUnpoisoned;

use super::{MemoryStore, replay};
use crate::ids::MessageId;
use crate::message::{AttachmentMessage, Message};
use crate::store::side_chat::{BOUNDARY, SOURCE, TITLE};
use crate::store::{
    Fact, ForkReceipt, SessionKind, SessionMeta, SideChatBase, SideChatFork, StoreError,
};

impl MemoryStore {
    pub(super) fn side_chat_fork(&self, fork: SideChatFork) -> Result<ForkReceipt, StoreError> {
        self.tracked(&fork.child.clone(), || {
            let mut sessions = self.sessions.lock_unpoisoned();
            // 同 node-store：`child` 已经是这个父会话分出的侧聊 → 什么都不写；是别的会话 → 拒绝
            if let Some((child, _)) = sessions.get(&fork.child) {
                return if child.kind == SessionKind::SideChat
                    && child.parent.as_ref() == Some(&fork.parent)
                {
                    Ok(ForkReceipt::Existing)
                } else {
                    Err(StoreError::Exists {
                        session: fork.child.clone(),
                    })
                };
            }
            let (parent, facts) =
                sessions
                    .get(&fork.parent)
                    .cloned()
                    .ok_or_else(|| StoreError::NotFound {
                        session: fork.parent.clone(),
                    })?;
            let (messages, _, _) = replay(&facts);
            let at = |id: &MessageId| messages.iter().position(|message| message.id() == id);
            let end = match &fork.base {
                SideChatBase::All => messages.len(),
                SideChatBase::Through(id) => {
                    at(id).ok_or_else(|| StoreError::Storage {
                        message: format!("Side chat base message not found in session store: {id}"),
                    })? + 1
                }
                SideChatBase::Before(id) => at(id).unwrap_or(messages.len()),
            };
            let meta = SessionMeta {
                id: fork.child.clone(),
                kind: SessionKind::SideChat,
                parent: Some(parent.id),
                workspace: parent.workspace,
                title: TITLE.to_owned(),
                trace: parent.trace,
                created_at_ms: fork.created_at_ms,
            };
            let boundary = Message::Attachment(AttachmentMessage {
                id: MessageId::new(format!("msg_side_chat_boundary_{}", fork.child)),
                source: SOURCE.to_owned(),
                text: rust_cli_base::reminder::wrap(BOUNDARY),
                data: serde_json::Value::Null,
            });
            let mut copied = vec![
                Fact::ModelSelected {
                    selection: fork.selection,
                },
                Fact::ExecutionChanged {
                    state: crate::permission::ExecutionState::from_parts(fork.mode, fork.plan),
                },
            ];
            copied.extend(
                messages[..end]
                    .iter()
                    .chain(std::iter::once(&boundary))
                    .map(|message| Fact::MessageAppended {
                        message: message.clone(),
                    }),
            );
            sessions.insert(fork.child, (meta, copied));
            Ok(ForkReceipt::Created { copied: end })
        })
    }
}
