//! 换掉一条输入（`Command::ReplaceInput`，docs/specs/rust-cli/session-lifecycle.md §10.53）：对话回退到这条输入之前（可以连同它起改过的文件），
//! 再以这条命令开新输入的回合。v5 的编辑重发与重试都是它。
//!
//! ```text
//! 空闲 ─► replace_now：恢复文件（代码回退任务，§5.2）─► 截对话 ─► 开回合 ─► 回执
//! 回合在跑 ─► 中止它（同停止）、记下 replacing ─► 回合结束 ─► start_next 先做 replace_now
//! stop_cut_works：截对话之前（带文件时是文件改回之后）停被截掉的回合里起的后台工作（replace_works.rs，§10.55）
//! ```
//!
//! 为什么在内核：前端拼「Rewind → Submit」时两步之间会话 actor 可能开了通知或定时任务的回合，扣住的队列还会让 Submit 被拒；回合在跑时
//! 前端要等它结束，等的那一段同样可能开别的回合。这里判断与执行在会话 actor 的一次处理里，或记成回合结束后第一件做的事。

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::input_extras::InputExtras;
use rust_cli_kernel_api::session_api::{Ack, CommandResult, RewindScope, RewoundFile};
use rust_cli_kernel_api::store::WorkspaceCheckpoint;

use crate::actor::{Actor, rejected};
use crate::rewind::Then;
use crate::rewind_plan;
use crate::turn::TurnInput;

/// 已有提升或换掉在等时的拒绝（只记一条，同 `Promote`）。
pub(crate) const REPLACEMENT_PENDING: &str =
    "another input is already waiting to start after this turn";
/// 目标不是可选的输入。
const NOT_SELECTABLE: &str = "is not a message to replace";

/// 一条要换掉的输入：命令、目标、新输入与回执。
pub(crate) struct Replacement {
    pub(crate) command: CommandId,
    pub(crate) target: MessageId,
    pub(crate) text: String,
    pub(crate) extras: Box<InputExtras>,
    pub(crate) restore_code: bool,
    /// 截对话之前停被截掉的回合里起的后台工作（§10.55）。
    pub(crate) stop_cut_works: bool,
    pub(crate) reply: crate::ledger::Reply,
}

impl Replacement {
    /// 新输入开的回合：输入的 id 就是这条命令的 id（回合记录、用户消息的来源命令、效果记录都是它）。
    fn input(self) -> (TurnInput, crate::ledger::Reply) {
        let input = TurnInput::Human {
            command: self.command,
            text: self.text,
            screened: None,
            more: Vec::new(),
            extras: self.extras,
        };
        (input, self.reply)
    }
}

impl Actor {
    /// `Command::ReplaceInput`：准入已放行（空闲，或回合在跑、没有压缩）。回执在新回合开了之后（[`Self::start_replacement`]）。
    pub(crate) async fn replace_input(&mut self, replacement: Replacement) {
        if !self
            .messages
            .iter()
            .any(|m| m.id() == &replacement.target && rewind_plan::selectable(m).is_some())
        {
            let reason = format!("{} {NOT_SELECTABLE}", replacement.target);
            self.commands
                .settle(replacement.reply, rejected(Refusal::NotFound, reason));
            return;
        }
        if self.promoted.is_some() || self.replacing.is_some() {
            self.commands.settle(
                replacement.reply,
                rejected(Refusal::Busy, REPLACEMENT_PENDING),
            );
            return;
        }
        if let Some(running) = &self.running {
            // 同停止（v4 的 stop barrier）：v5 的组装因此扣住排队的输入；这个回合的消息随后被截掉，中断标记不留在对话里
            running.abort.abort(AbortReason::User);
            self.cancel_turn_asks();
            self.replacing = Some(Box::new(replacement));
            return;
        }
        self.replace_now(replacement).await;
    }

    /// 空闲（或被中止的回合刚结束）：恢复文件（要的话）、截对话、开新回合。
    pub(crate) async fn replace_now(&mut self, replacement: Replacement) {
        let undo: Vec<WorkspaceCheckpoint> = if replacement.restore_code {
            rewind_plan::checkpoints_from(&self.messages, &self.checkpoints, &replacement.target)
                .into_iter()
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        // target 起没有 checkpoint 时只回退对话（同 `Rewind{Both}`）
        if undo.is_empty() {
            if replacement.stop_cut_works {
                self.stop_cut_works(&replacement.target).await;
            }
            let cut = self
                .rewind_conversation(
                    Some(replacement.command.clone()),
                    &replacement.target,
                    Vec::new(),
                )
                .await;
            match cut {
                Ok(removed) => {
                    self.start_replacement(replacement, Vec::new(), removed)
                        .await
                }
                Err(reason) => self
                    .commands
                    .settle(replacement.reply, rejected(Refusal::Failed, reason)),
            }
            return;
        }
        let (command, target) = (replacement.command.clone(), replacement.target.clone());
        self.rewind_code(
            command,
            target,
            RewindScope::Both,
            undo,
            Then::Replace(Box::new(replacement)),
        );
    }

    /// 对话已经截了：开新输入的回合，再回执（`TurnStarted` 与输入的结局先进事件日志，§15.4.2 的 G2）。
    pub(crate) async fn start_replacement(
        &mut self,
        replacement: Replacement,
        restored: Vec<RewoundFile>,
        removed: usize,
    ) {
        let (input, reply) = replacement.input();
        let ack = match self.start_turn(input).await {
            Ok(()) => Ack::Accepted {
                result: CommandResult::Rewound { restored, removed },
            },
            Err(error) => rejected(Refusal::Failed, error.to_string()),
        };
        self.commands.settle(reply, ack);
    }
}
