use std::collections::HashSet;
use std::sync::atomic::Ordering;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::input_journal::is_journal_input;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::session_api::{Ack, Command, CommandEnvelope, Delivery, QueuedInput};
use rust_cli_kernel_api::store::StoreError;

use crate::actor::{Actor, rejected};
use crate::inbox::InputKind;

/// 输入账本里有记录的命令（pg-store.md §4）：经 [`Actor::admit_input`] 接纳的，与重开时从账本恢复的。提升与取消只对
/// 它们记账：SessionStart 给的首条输入、排队的压缩、不进账本的命令不在账本里，持久账本对不认识的命令报冲突。
///
/// 修 bug（合并 v5 时）：以前按命令 id 的前缀认出 SessionStart 的首条输入（`session-start-`），v5 的命令 id 一律由一个
/// 生成器给（UUIDv7，没有前缀），这条输入的提升会让持久账本报冲突、整批提交失败。现在会话 actor 记下自己记过账的命令，
/// 不再从 id 猜。本机的账本不记账，这里记了也不改变行为。
#[derive(Default)]
pub(crate) struct Journaled(HashSet<CommandId>);

impl Journaled {
    /// 只留记过账的命令（顺序不变）。
    pub(crate) fn retain(&self, commands: Vec<CommandId>) -> Vec<CommandId> {
        commands
            .into_iter()
            .filter(|command| self.0.contains(command))
            .collect()
    }
}

impl Actor {
    pub(crate) fn store_failed(&self, error: StoreError) -> String {
        self.pause_writes(error).to_string()
    }

    /// 存储说“结果未知”或“暂时不可用”时暂停本会话的写入，原样返回错误。只看错误的种类，不问是哪种库：只有一个写者的
    /// 本机库不会给出这两种错误。
    pub(crate) fn pause_writes(&self, error: StoreError) -> StoreError {
        if matches!(
            error,
            StoreError::OutcomeUnknown | StoreError::Unavailable { .. }
        ) {
            // 提交结果未确认时不能让下一条输入继续推进旧内存状态；重新打开会话后从事实恢复。
            self.write_blocked.store(true, Ordering::SeqCst);
        }
        error
    }

    pub(crate) async fn admit_input(&mut self, envelope: &CommandEnvelope) -> Option<Ack> {
        if !is_journal_input(&envelope.command) {
            return None;
        }
        if self.write_blocked.load(Ordering::SeqCst) {
            return Some(rejected(
                Refusal::Failed,
                "session storage is paused; reopen the session to reconcile committed facts",
            ));
        }
        match self.deps.journal.admit(&self.meta.id, envelope).await {
            // 别的进程接纳过的同一个命令（会话重开之后重发）：回它原来的回执（session-lifecycle.md §10.3）。
            Ok(admission) if admission.duplicate => Some(Ack::Duplicate {
                original: Box::new(admission.input.receipt),
            }),
            Ok(_) => {
                self.journaled.0.insert(envelope.id.clone());
                None
            }
            Err(error @ StoreError::OutcomeUnknown) => Some(Ack::OutcomeUnknown {
                reason: self.store_failed(error),
            }),
            // 同一个命令 id 已经记成另一条输入（pg-store.md §4）：与本进程认出的冲突是同一种拒绝。
            Err(error @ StoreError::Conflict { .. }) => Some(rejected(
                Refusal::CommandIdConflict,
                self.store_failed(error),
            )),
            Err(error) => Some(rejected(Refusal::Failed, self.store_failed(error))),
        }
    }

    pub(crate) fn restore_input(&mut self, envelope: CommandEnvelope) {
        let id = envelope.id;
        let (queued, kind) = match envelope.command {
            // 正文之外的事实随输入一起记在账本里（session-lifecycle.md §10.14），回到队列时照原样带上。
            Command::Submit { text, extras, .. } => (
                QueuedInput {
                    extras,
                    ..QueuedInput::new(id, text, Delivery::Queue)
                },
                InputKind::Text,
            ),
            Command::Bash { command, .. } => (
                QueuedInput::new(
                    id,
                    rust_cli_kernel_api::user_input::bash_input(&command),
                    Delivery::Queue,
                ),
                InputKind::Bash(command),
            ),
            Command::SubmitContent { content, .. } => {
                let text = content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (
                    QueuedInput::new(id, text, Delivery::Queue),
                    InputKind::Content(content),
                )
            }
            _ => return,
        };
        // 冷恢复时没有旧的工具边界，原 steer 输入按持久接纳顺序回到队列。
        self.journaled.0.insert(queued.command.clone());
        self.inbox.push_input(queued, kind);
        self.emit_queue();
    }

    /// 一条排队的输入不再执行（用户取回、取空扣住的队列、工具边界丢弃）：先在账本里记取消，确认之后才从内存队列去掉，
    /// 返回去掉的那一项。账本没有确认时这条留在队列里（账本里仍是待处理）。丢弃排队输入的地方都经这里：账本与内存队列是
    /// 同一条输入的两份记录，只在这一处一起改。不在队列里时返回 None，什么都不做。
    pub(crate) async fn discard_input(
        &mut self,
        command: &CommandId,
    ) -> Result<Option<crate::inbox::Queued>, StoreError> {
        if self.inbox.kind_of(command).is_none() {
            return Ok(None);
        }
        self.cancel_durable_input(command).await?;
        Ok(self.inbox.remove_input(command))
    }

    /// 只记账本的取消：输入已经出了队（开了回合、被 UserPromptSubmit 拦下）时用；还在队列里的用 [`Self::discard_input`]。
    /// 账本里没有这条（[`Journaled`]）时什么都不做。
    pub(crate) async fn cancel_durable_input(&self, command: &CommandId) -> Result<(), StoreError> {
        if !self.journaled.0.contains(command) {
            return Ok(());
        }
        self.deps
            .journal
            .cancel(&self.meta.id, std::slice::from_ref(command))
            .await
    }
}
