//! 会话的输入账本（pg-store.md §4）：已接纳、还没变成消息的输入记在哪里。
//!
//! 它是独立的端口，由组装根注入（pg-store 的会话库实现它；本机没有持久账本，注入 [`NoJournal`]）。会话 actor 对两种
//! 实现走同一条路径：接纳先记账本、再进内存队列；丢弃先记取消、再出队。内核不问“有没有账本”。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ids::{CommandId, MessageId, SessionId};
use crate::session_api::{Ack, Command, CommandEnvelope};
use crate::store::{Fact, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputState {
    Pending,
    Promoted,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalInput {
    pub envelope: CommandEnvelope,
    pub sequence: u64,
    pub state: InputState,
    pub receipt: Ack,
    pub promoted_message: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputAdmission {
    pub input: JournalInput,
    pub duplicate: bool,
}

pub fn is_journal_input(command: &Command) -> bool {
    matches!(
        command,
        Command::Submit { .. } | Command::SubmitContent { .. } | Command::Bash { .. }
    )
}

/// 方法都是必需的：包装层漏转发一个就编译失败。
#[async_trait]
pub trait InputJournal: Send + Sync {
    async fn admit(
        &self,
        session: &SessionId,
        envelope: &CommandEnvelope,
    ) -> Result<InputAdmission, StoreError>;
    async fn pending(&self, session: &SessionId) -> Result<Vec<JournalInput>, StoreError>;
    async fn input(
        &self,
        session: &SessionId,
        command: &CommandId,
    ) -> Result<Option<JournalInput>, StoreError>;
    async fn cancel(&self, session: &SessionId, commands: &[CommandId]) -> Result<(), StoreError>;

    /// 这些输入变成了 `message`：要随这条消息放进同一批提交的事实。持久账本给出 [`Fact::InputsPromoted`]，会话库在
    /// 同一个事务里把它们标为已提升（pg-store.md §4）；不记账的账本没有要提升的，给空。会话 actor 照给出的放进批次，
    /// 自己不判断账本的种类。
    fn promotion(&self, commands: Vec<CommandId>, message: &MessageId) -> Vec<Fact>;
}

/// 持久账本的 [`InputJournal::promotion`]：没有命令时不出事实。
pub fn promoted(commands: Vec<CommandId>, message: &MessageId) -> Vec<Fact> {
    if commands.is_empty() {
        return Vec::new();
    }
    vec![Fact::InputsPromoted {
        commands,
        message: message.clone(),
    }]
}

/// 不持久的账本：输入只在会话 actor 的内存队列里（本机的会话库、测试）。接纳总是通过、不算重复（同一个命令 ID 的重复
/// 由会话 actor 自己的已见集合挡住），重开会话后没有待处理的输入，取消什么都不做。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoJournal;

#[async_trait]
impl InputJournal for NoJournal {
    async fn admit(
        &self,
        _session: &SessionId,
        envelope: &CommandEnvelope,
    ) -> Result<InputAdmission, StoreError> {
        Ok(InputAdmission {
            input: JournalInput {
                envelope: envelope.clone(),
                sequence: 0,
                state: InputState::Pending,
                receipt: Ack::Accepted {
                    result: crate::session_api::CommandResult::Done,
                },
                promoted_message: None,
            },
            duplicate: false,
        })
    }

    async fn pending(&self, _session: &SessionId) -> Result<Vec<JournalInput>, StoreError> {
        Ok(Vec::new())
    }

    async fn input(
        &self,
        _session: &SessionId,
        _command: &CommandId,
    ) -> Result<Option<JournalInput>, StoreError> {
        Ok(None)
    }

    async fn cancel(
        &self,
        _session: &SessionId,
        _commands: &[CommandId],
    ) -> Result<(), StoreError> {
        Ok(())
    }

    fn promotion(&self, _commands: Vec<CommandId>, _message: &MessageId) -> Vec<Fact> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_volatile_journal_admits_everything_and_remembers_nothing() {
        let session = SessionId::new("sess_1");
        let envelope = CommandEnvelope {
            id: CommandId::new("cmd_1"),
            command: Command::Interrupt { target: None },
        };
        let journal = NoJournal;
        for _ in 0..2 {
            let admitted = journal.admit(&session, &envelope).await.unwrap();
            assert!(!admitted.duplicate);
            assert_eq!(admitted.input.envelope, envelope);
        }
        assert!(journal.pending(&session).await.unwrap().is_empty());
        assert_eq!(journal.input(&session, &envelope.id).await.unwrap(), None);
        journal
            .cancel(&session, std::slice::from_ref(&envelope.id))
            .await
            .unwrap();
        let message = MessageId::new("msg_1");
        assert!(
            journal
                .promotion(vec![envelope.id.clone()], &message)
                .is_empty()
        );
        assert_eq!(
            promoted(vec![envelope.id.clone()], &message),
            vec![Fact::InputsPromoted {
                commands: vec![envelope.id],
                message: message.clone(),
            }]
        );
        assert!(promoted(Vec::new(), &message).is_empty());
    }
}
