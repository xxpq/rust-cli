//! 改排队的项、重排与编辑锁（docs/specs/rust-cli/session-lifecycle.md §10.21、§10.30、§10.59，v5 的 `queue.edit` / `queue.reorder` /
//! `queue.lockEdit` / `queue.unlockEdit` / `queue.applyConfig`）。
//!
//! 改的前置条件在这里判，两种（kernel-api `EditGuard`）：前端看到的整项（`EditGuard::Seen`：投影可能落后于内核，前端把与那个版本对应的整项交下来，
//! actor 在同一次处理里比，不同就什么都不改；位置不是项的字段，别的项出队、挪动不让它被拒），或持锁的编辑器（`EditGuard::Lock`：锁住之后只有它改这一项，
//! 不比内容）。挪动只改顺序，前置条件只有项与锚点都在。出队也在 actor 里，所以一项要么还在、要么已经出队。
//!
//! 锁是排队项的字段（`QueuedInput.edit_lock`），与项同生同灭；锁着的项不出队（inbox.rs 的 `pop_later`），这里只管加锁、放开与被锁挡住的命令。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::input_extras::TurnConfig;
use rust_cli_kernel_api::session_api::{Ack, CommandResult, EditGuard, QueueEditLock, QueuedEdit};

use crate::actor::{Actor, accepted, rejected};
use crate::inbox::InputKind;
use crate::lane::ALREADY_SENT;

/// 前端看到的这一项已经变了（`Rejected.reason`，给人看；前端按 `Refusal::Stale` 分支）。
const STALE: &str = "the queued item changed since it was shown";
/// 只有普通文字的排队输入能改正文（`EditQueued` 的 `Rejected.reason`）。
const NOT_EDITABLE: &str = "only a queued text input can be edited";
/// 只有排在回合之后的普通文字能锁（`LockQueued` 的 `Rejected.reason`）：插话在下一个工具边界就并入，锁不住。
const NOT_LOCKABLE: &str = "only a text input queued after the turn can be locked for editing";
/// 这一项被编辑器锁着（`Rejected.reason`；前端按 `Refusal::QueueItemLocked` 分支）。
const LOCKED: &str = "the queued item is being edited";
/// 带锁改时锁已不是这个编辑器的（`Rejected.reason`；前端按 `Refusal::EditLockLost` 分支）。
const LOCK_LOST: &str = "the edit lock on the queued item is no longer held by this editor";
/// 重排的锚点已经不在队列里（`MoveQueued` 的 `Rejected.reason`）。
const ANCHOR_GONE: &str = "the item to move before is no longer queued";

fn unchanged() -> Ack {
    Ack::Accepted {
        result: CommandResult::Unchanged,
    }
}

impl Actor {
    /// `Command::EditQueued`：给了的字段原位换掉，并入点筛查的结果随之作废（inbox.rs）。`EditGuard::Lock` 时同一步放开锁，空闲就接着开下一项。
    ///
    /// 修 bug（13 Q24，§10.30）：以前比（正文，位置），排在前面的项出队、被挪走之后位置变了，用户正在改的这一项被拒；
    /// 被拒的理由不是「两边改了同一项」。位置不再进比较；整项（含执行配置）与前端判的版本是同一份内容。
    pub(crate) async fn edit_queued(
        &mut self,
        command: &CommandId,
        edit: &QueuedEdit,
        guard: &EditGuard,
    ) -> Ack {
        let Some(queued) = self.inbox.entry(command) else {
            return rejected(Refusal::NotFound, ALREADY_SENT);
        };
        let current = &queued.input;
        match guard {
            EditGuard::Lock { editor }
                if current.edit_lock.as_ref().map(|lock| &lock.editor) != Some(editor) =>
            {
                return rejected(Refusal::EditLockLost, LOCK_LOST);
            }
            EditGuard::Seen { .. } if current.edit_lock.is_some() => {
                return rejected(Refusal::QueueItemLocked, LOCKED);
            }
            EditGuard::Seen { expected } if current != expected.as_ref() => {
                return rejected(Refusal::Stale, STALE);
            }
            EditGuard::Lock { .. } | EditGuard::Seen { .. } => {}
        }
        if !matches!(queued.kind, InputKind::Text) {
            return rejected(Refusal::Unavailable, NOT_EDITABLE);
        }
        let before = current.clone();
        self.inbox.edit(command, edit);
        let release = matches!(guard, EditGuard::Lock { .. });
        if release {
            self.inbox.set_lock(command, None);
        }
        if self.inbox.queued_input(command) != Some(&before) {
            self.emit_queue();
        }
        if release {
            self.resume_after_unlock().await;
        }
        accepted()
    }

    /// `Command::LockQueued`：锁住一条排在回合之后的文字输入。同一个编辑器再锁是 `Accepted`、不改状态（照 v4：界面只把 accepted 当加锁成功）。
    pub(crate) fn lock_queued(&mut self, command: &CommandId, editor: String) -> Ack {
        let Some(queued) = self.inbox.entry(command) else {
            return rejected(Refusal::NotFound, ALREADY_SENT);
        };
        if queued.in_lane() || !matches!(queued.kind, InputKind::Text) {
            return rejected(Refusal::Unavailable, NOT_LOCKABLE);
        }
        match &queued.input.edit_lock {
            Some(lock) if lock.editor == editor => return accepted(),
            Some(_) => return rejected(Refusal::QueueItemLocked, LOCKED),
            None => {}
        }
        self.inbox.set_lock(command, Some(QueueEditLock { editor }));
        self.emit_queue();
        accepted()
    }

    /// `Command::UnlockQueued`：谁都能放开（发起端取消、刷新后放开遗留的锁、别的端解除）。没有锁时什么都不做。
    pub(crate) async fn unlock_queued(&mut self, command: &CommandId) -> Ack {
        if !self.queued_locked(command) {
            return unchanged();
        }
        self.inbox.set_lock(command, None);
        self.emit_queue();
        self.resume_after_unlock().await;
        accepted()
    }

    /// 锁着的队首只因为锁而没开（inbox.rs 的 `pop_later`）：放开之后空闲就开下一项。扣住时 `start_next` 只开通知，不越过扣住。
    async fn resume_after_unlock(&mut self) {
        if !self.busy() {
            self.start_next().await;
        }
    }

    /// `Command::ApplyQueuedConfig`：逐项换模型与（或）权限模式；不在、锁着、不是普通文字的跳过（照 v4，用户的意图只针对仍在等的项）。
    pub(crate) fn apply_queued_config(
        &mut self,
        commands: &[CommandId],
        config: &TurnConfig,
    ) -> Ack {
        let mut changed = false;
        for command in commands {
            let applicable = self.inbox.entry(command).is_some_and(|queued| {
                queued.input.edit_lock.is_none() && matches!(queued.kind, InputKind::Text)
            });
            changed |= applicable && self.inbox.reconfigure(command, config);
        }
        if !changed {
            return unchanged();
        }
        self.emit_queue();
        accepted()
    }

    /// 这一项被编辑器锁着（取回、挪动、提升之前判，§10.59）。
    pub(crate) fn queued_locked(&self, command: &CommandId) -> bool {
        self.inbox
            .entry(command)
            .is_some_and(|queued| queued.input.edit_lock.is_some())
    }

    /// `Command::MoveQueued`：挪到 `before` 之前（None：挪到最后）。`before` 是它自己时不变。只改顺序、不读项的内容，所以不比内容
    /// （§10.30）：两条挪同一项的命令按 mailbox 的先后，后到的胜。锁着的项不挪（§10.59）。
    pub(crate) fn move_queued(&mut self, command: &CommandId, before: Option<&CommandId>) -> Ack {
        if self.inbox.queued_input(command).is_none() {
            return rejected(Refusal::NotFound, ALREADY_SENT);
        }
        if self.queued_locked(command) {
            return locked();
        }
        if before == Some(command) {
            return accepted();
        }
        if let Some(anchor) = before
            && self.inbox.queued_input(anchor).is_none()
        {
            return rejected(Refusal::NotFound, ANCHOR_GONE);
        }
        let order = self.inbox.queued();
        self.inbox.move_before(command, before);
        if self.inbox.queued() != order {
            self.emit_queue();
        }
        accepted()
    }
}

/// 这一项被编辑器锁着时的拒绝（lane.rs 的取回与提升共用）。
pub(crate) fn locked() -> Ack {
    rejected(Refusal::QueueItemLocked, LOCKED)
}

#[cfg(test)]
#[path = "queue_edit_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "queue_lock_tests.rs"]
mod lock_tests;
