//! next 通道（L18，docs/specs/rust-cli/query.md §5）：插话输入与两种后台通知按到达顺序排在一起。
//!
//! 插话、后台命令通知与子代理通知都是优先级 `next`，工具边界按队列顺序取，回合
//! 结束后也按这个顺序开回合；`Queue` 输入是 `later`，不进通道。通道与排队的输入是 actor 的 `Inbox`（inbox.rs）
//! 的两个投影：一条输入只存一份。
//!
//! 修正（L18）：以前通知只在回合结束后各开一个回合（按缺省优先级 `later`
//! 理解），排在全部排队输入之后；通知应是 `next`，在下一个工具边界并入。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::agents::AgentNotice;
use rust_cli_kernel_api::background::NoticedWork;
use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::machine_notices::MachineNotice;
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::session_api::{DropReason, InputFate, QueueHold, QueuedInput};
use rust_cli_kernel_api::store::Fact;

use crate::actor::Actor;
use crate::inbox::{InputKind, Item, LaneItem, Queued, is_slash};
use crate::notices::{NOTICE_SOURCE, task_notification};
use crate::peers::{PEER_SOURCE, PeerItem};
use crate::turn::TurnInput;

/// 取回时输入已经不在队列里（`Command::Dequeue` 的 `Rejected.reason`，给用户看）。
pub(crate) const ALREADY_SENT: &str = "the message already went out";
/// 提升的输入不是普通文字或压缩（`Command::Promote` 的 `Rejected.reason`）。
const NOT_TEXT: &str = "only a text input can be sent now";
/// 已经有一条提升的输入等着它的回合结束（`Command::Promote` 的 `Rejected.reason`）。
const PROMOTION_PENDING: &str = "another queued input is already waiting to be sent now";
/// 命令 id 按字面排好（集合比较用）。
fn sorted(ids: &[CommandId]) -> Vec<&str> {
    let mut ids: Vec<&str> = ids.iter().map(CommandId::as_str).collect();
    ids.sort_unstable();
    ids
}

/// 处置扣住的队列时队列已经变了（§10.16）：给人看的文字，前端按 `Refusal::HeldQueueChanged` 分支。
const HELD_QUEUE_CHANGED: &str = "the held queue changed since it was shown";

/// 一条等候中的后台通知，或跨会话的消息与回执（同伴消息同样是优先级 `next`，cross-session.md §1.7）。
#[derive(Debug, Clone)]
pub(crate) enum Waiting {
    /// 后台命令的终态通知，带它所在的机器（R3：ack 发给那台机器）。
    Shell(MachineNotice),
    Agent(AgentNotice),
    Peer(PeerItem),
    /// Monitor 的事件或说明（工作线 F83，tools-more.md §19.6）：actor 自己产生，全文已包装；不 ack，也不按文字去重
    /// （同样的一行可以出现多次）。
    MonitorEvent(String),
    /// Monitor 的终态通知（带剩下的行），提交后 ack 它的 `ShellNotice`。
    MonitorEnd {
        notice: Box<MachineNotice>,
        text: String,
    },
}

impl Waiting {
    /// 通知全文（`<system-reminder>` 包装）：工具边界并入用它；后台通知开回合也用它。
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Shell(notice) => task_notification(&notice.notice),
            Self::Agent(notice) => notice.text.clone(),
            Self::Peer(item) => item.text(),
            Self::MonitorEvent(text) | Self::MonitorEnd { text, .. } => text.clone(),
        }
    }

    /// 空闲时开回合的 user 消息文字（同伴消息的第一行不同）。
    pub(crate) fn turn_text(&self) -> String {
        match self {
            Self::Peer(item) => item.turn_text(),
            _ => self.text(),
        }
    }

    /// 开回合的 meta user 消息的来源。
    pub(crate) fn source(&self) -> &'static str {
        match self {
            Self::Peer(_) => PEER_SOURCE,
            _ => NOTICE_SOURCE,
        }
    }

    /// 开回合的后台工作（session-lifecycle.md §10.50）：命令与 monitor 的终态通知自带任务 ID、种类与说明；子代理的通知只带 ID，说明按会话的后台
    /// 工作表找（结束了的项也记着，通知开回合时它多半已经结束），找不到时为空。同伴的消息与 monitor 的事件行不是一项后台工作的终态，没有。
    pub(crate) fn work(&self, works: &crate::works::Works) -> Option<NoticedWork> {
        let shell = |notice: &rust_cli_kernel_api::env_shell::ShellNotice| NoticedWork {
            id: notice.task.as_str().to_owned(),
            kind: notice.kind.into(),
            description: notice.description.clone(),
        };
        match self {
            Self::Shell(notice) => Some(shell(&notice.notice)),
            Self::MonitorEnd { notice, .. } => Some(shell(&notice.notice)),
            Self::Agent(notice) => {
                let id = notice.agent.to_string();
                Some(NoticedWork {
                    description: works.description(&id).unwrap_or_default().to_owned(),
                    id,
                    kind: BackgroundKind::Agent,
                })
            }
            Self::Peer(_) | Self::MonitorEvent(_) => None,
        }
    }

    /// 这条通知是哪项后台工作的终态（任务 ID 或子代理 ID）；同伴的消息与 monitor 的事件行不是，为 None。
    pub(crate) fn work_id(&self) -> Option<String> {
        match self {
            Self::Shell(notice) => Some(notice.notice.task.to_string()),
            Self::MonitorEnd { notice, .. } => Some(notice.notice.task.to_string()),
            Self::Agent(notice) => Some(notice.agent.to_string()),
            Self::Peer(_) | Self::MonitorEvent(_) => None,
        }
    }

    /// 只在工具边界并入、不开回合（`-p` 的回执，passive）。
    pub(crate) fn passive(&self) -> bool {
        matches!(self, Self::Peer(item) if item.passive())
    }

    /// 并入对话时与那条消息同一批提交的事实：同伴的消息与回执是 `peer_merged`（cross-session.md §2.4，F139），其余没有。
    pub(crate) fn merged_facts(&self) -> Vec<Fact> {
        match self {
            Self::Peer(item) => item.merged_facts(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Shell(a), Self::Shell(b)) => {
                a.machine == b.machine && a.notice.seq == b.notice.seq
            }
            (Self::Agent(a), Self::Agent(b)) => a.seq == b.seq,
            (Self::Peer(a), Self::Peer(b)) => a == b,
            (Self::MonitorEnd { notice: a, .. }, Self::MonitorEnd { notice: b, .. }) => {
                a.machine == b.machine && a.notice.seq == b.notice.seq
            }
            _ => false,
        }
    }
}

/// 消息里的通知全文：开回合的 meta user 消息，或工具边界并入的 `task-notification` attachment。
fn notice_text(message: &Message) -> Option<&str> {
    match message {
        Message::User(user) if matches!(&user.origin, UserOrigin::Meta { source } if source == NOTICE_SOURCE || source == PEER_SOURCE) => {
            match user.content.as_slice() {
                [ContentBlock::Text { text }] => Some(text),
                _ => None,
            }
        }
        Message::Attachment(attachment)
            if attachment.source == queued_command::SOURCE
                && attachment.data[queued_command::MODE_KEY] == NOTICE_SOURCE =>
        {
            Some(&attachment.text)
        }
        _ => None,
    }
}

impl Actor {
    /// 通知到达：已在对话里或已在通道里（重复送达）时只 ack；否则排进通道。返回是否新排进。
    pub(crate) async fn enqueue_notice(&mut self, notice: Waiting) -> bool {
        let text = notice.text();
        let seen = self
            .messages
            .iter()
            .any(|message| notice_text(message) == Some(text.as_str()));
        let waiting = self.inbox.has_notice(&notice);
        if seen || waiting {
            if !waiting {
                self.ack_notice(&notice).await;
            }
            return false;
        }
        self.inbox.push_notice(notice);
        true
    }

    /// 提交之后才 ack（至少一次）。同伴的消息与回执在收到时已经提交并 ack（peers.rs）。
    pub(crate) async fn ack_notice(&self, notice: &Waiting) {
        match notice {
            Waiting::Shell(notice) => self.ack(notice).await,
            Waiting::MonitorEnd { notice, .. } => self.ack(notice).await,
            Waiting::Agent(notice) => self.ack_agent(notice.seq).await,
            Waiting::Peer(_) | Waiting::MonitorEvent(_) => {}
        }
    }

    /// 工具边界并入的 `task-notification` 已提交：按交给主循环时给的消息 ID 从通道取走这条通知并 ack（工作线 F130：
    /// 以前按 attachment 的文字与通知全文相等找回）。
    pub(crate) async fn notice_absorbed(&mut self, message: &Message) {
        let Message::Attachment(attachment) = message else {
            return;
        };
        if let Some(notice) = self.inbox.take_absorbed(&attachment.id) {
            self.ack_notice(&notice).await;
        }
    }

    /// 回合结束后与头一条输入成批的后续输入（修复线 L-A，query.md §4.3，同对照实现）：头一条是
    /// 普通文字、不是斜杠命令时，往后连着取通道里的插话、再取队列里的 `Queue` 输入；遇到通知、斜杠命令、bash、
    /// 带图片的输入、含 `@` 的文字就停（留在原处）。
    ///
    /// 配置了 UserPromptSubmit 时不成批（暂定）：对照实现在入队时就逐条筛查，本工程在开回合的任务里只筛查头一条，头一条被
    /// 阻塞时成批的后续输入会跟着丢掉。
    ///
    /// 自己一个回合的输入（SessionStart 给的首条输入，inbox.rs 的 `solo`）不成批。
    fn take_batch(&mut self, head: &Queued) -> Vec<crate::turn::MorePrompt> {
        let screening = self
            .deps
            .hooks
            .has_hooks(rust_cli_kernel_api::hooks::HookEventName::UserPromptSubmit);
        // 带正文之外事实的输入自己一轮（session-lifecycle.md §10.14）：它的材料与记录挂在开回合的输入上。
        if is_slash(&head.input.text) || screening || head.solo || head.input.extras.own_turn() {
            return Vec::new();
        }
        let mut more = Vec::new();
        while let Some(queued) = self.inbox.take_batched(&head.input.extras.config) {
            let text = queued.input.text.clone();
            more.push(crate::turn::MorePrompt {
                id: self.deps.ids.message(),
                command: queued.input.command.clone(),
                screened: queued.screened(),
                text,
            });
        }
        if !more.is_empty() {
            self.emit_queue();
        }
        more
    }

    /// `Command::Dequeue`（query.md §4.4）：还在队列里的输入取回——从队列与通道去掉（输入的种类与并入点筛查的结果
    /// 跟着条目一起去掉）。判断与出队在 actor 的一次处理里：开回合、成批与工具边界的结局（`settle_steering`）也都在
    /// actor 里出队，所以一条输入要么取回、要么发出，不会两样都有。
    ///
    /// 缺陷修复：以前只从内存队列去掉，输入账本里这条仍是待处理——有持久账本时，重开会话后它回到队列并被执行。原因是
    /// 出队的几处各自记得（或忘了）同步账本；现在与工具边界的丢弃一样经 `discard_input`，先记取消、再出队。账本没有
    /// 确认时回 `Rejected`，这条留在队列里。
    pub(crate) async fn dequeue(
        &mut self,
        command: &CommandId,
    ) -> rust_cli_kernel_api::session_api::Ack {
        // 锁着的项只能由编辑提交或放开锁改变（§10.59，照 v4）：单项取回不越过锁
        if self.queued_locked(command) {
            return crate::queue_edit::locked();
        }
        self.take_back(command).await
    }

    /// 取回一条排队的项（[`Self::dequeue`] 与取空扣住的队列共用）：不看锁。
    async fn take_back(&mut self, command: &CommandId) -> rust_cli_kernel_api::session_api::Ack {
        let queued = match self.discard_input(command).await {
            Ok(Some(queued)) => queued,
            Ok(None) => return crate::actor::rejected(Refusal::NotFound, ALREADY_SENT),
            Err(error) => return crate::actor::rejected(Refusal::Failed, self.store_failed(error)),
        };
        self.emit_queue();
        // 排队的压缩不是输入，没有输入的结局（§10.23）
        if !matches!(queued.kind, InputKind::Compact) {
            self.input_fate(
                command.clone(),
                InputFate::Dropped {
                    why: DropReason::TakenBack,
                },
            );
        }
        // 停止或失败扣住的队列取空了：扣住随之放开（扣住只对队列里有输入有意义，§10.6）。用户要求的扣住保留。
        if matches!(
            self.queue_hold,
            Some(QueueHold::Stopped | QueueHold::TurnFailed)
        ) && !self.inbox.has_inputs()
        {
            self.hold_queue(None);
        }
        crate::actor::accepted()
    }

    /// 一条文字输入（`Submit` 与 v5 的两种处置，§10.14、§10.16）：准入给的是排队就进队列（插话进 next 通道，由主循环在下一个工具
    /// 边界领取，steering.rs），否则现在开回合。
    pub(crate) async fn submit(
        &mut self,
        command: CommandId,
        text: String,
        extras: rust_cli_kernel_api::input_extras::InputExtras,
        admission: rust_cli_kernel_api::admission::Admission,
    ) -> rust_cli_kernel_api::session_api::Ack {
        if let rust_cli_kernel_api::admission::Admission::Enqueue(delivery) = admission {
            self.inbox.push_input(
                QueuedInput {
                    extras,
                    ..QueuedInput::new(command.clone(), text, delivery)
                },
                InputKind::Text,
            );
            self.emit_queue();
            self.input_fate(command, InputFate::Queued);
            return crate::actor::accepted();
        }
        let input = TurnInput::Human {
            command,
            text,
            screened: None,
            more: Vec::new(),
            extras: Box::new(extras),
        };
        match self.start_turn(input).await {
            Ok(()) => crate::actor::accepted(),
            Err(reason) => crate::actor::rejected(Refusal::Failed, reason),
        }
    }

    /// 排队的输入扣住了、队列里还有输入（§10.6；准入的 `held` 与 v5 的处置都看它）。
    pub(crate) fn queue_is_held(&self) -> bool {
        self.queue_hold.is_some() && self.inbox.has_inputs()
    }

    /// 前端看到的扣住的队列与现在的相符（session-lifecycle.md §10.16，v5 的 `expectedHeldQueueItemIds`）：按集合比（同 v4）；
    /// 队列没有扣住、或没给时不查。不符时什么都不做：前端按 `current` 重新问用户。
    pub(crate) fn held_as_expected(
        &self,
        expected: Option<&[CommandId]>,
    ) -> Result<(), rust_cli_kernel_api::session_api::Ack> {
        let Some(expected) = expected.filter(|_| self.queue_is_held()) else {
            return Ok(());
        };
        let current: Vec<CommandId> = self
            .inbox
            .queued()
            .into_iter()
            .map(|input| input.command)
            .collect();
        if sorted(expected) == sorted(&current) {
            return Ok(());
        }
        Err(crate::actor::rejected(
            Refusal::HeldQueueChanged { current },
            HELD_QUEUE_CHANGED,
        ))
    }

    /// 取空扣住的队列（§10.16，v5 的 `discardQueue`）：每条照 `Dequeue` 取回（`Dropped{TakenBack}`），取空时停止或失败的扣住随之放开。
    /// 队列没有扣住时什么都不取（这条处置只对扣住的队列有意义，同 v4）。锁着的项一起取走（§10.59，照 v4：这是用户对整个队列的处置；
    /// 发起编辑的一端从投影看到项没了，进入失效态）。
    pub(crate) async fn discard_held(&mut self) {
        if !self.queue_is_held() {
            return;
        }
        for input in self.inbox.queued() {
            self.take_back(&input.command).await;
        }
    }

    /// `Command::Promote`（session-lifecycle.md §10.13，v5 的 `queue.sendNow`）：准入已判过指名的回合。这条输入出队，自己一轮
    /// （不与别的输入成批）：空闲时立即开；回合在跑时中止它，记下这一条，回合结束后 `start_next` 先开它（不论扣住）。
    /// 出队与开回合（或记下）在 actor 的一次处理里，所以一条输入要么还在队列、要么已经提升，不会两样都有。排队的压缩（§10.23）
    /// 同样提升：开的是压缩的回合。
    pub(crate) async fn promote(
        &mut self,
        command: CommandId,
    ) -> rust_cli_kernel_api::session_api::Ack {
        match self.inbox.kind_of(&command) {
            None => return crate::actor::rejected(Refusal::NotFound, ALREADY_SENT),
            Some(InputKind::Text | InputKind::Compact) => {}
            Some(InputKind::Bash(_) | InputKind::Content(_)) => {
                return crate::actor::rejected(Refusal::Unavailable, NOT_TEXT);
            }
        }
        // 锁着的项不提升（§10.59，照 v4）：正在改的内容还没提交
        if self.queued_locked(&command) {
            return crate::queue_edit::locked();
        }
        // 只记一条：第二条提升在前一条的回合结束之前到，会覆盖它、让它从队列里消失
        if self.promoted.is_some() || self.replacing.is_some() {
            return crate::actor::rejected(Refusal::Busy, PROMOTION_PENDING);
        }
        let Some(queued) = self.inbox.remove_input(&command) else {
            return crate::actor::rejected(Refusal::NotFound, ALREADY_SENT);
        };
        self.emit_queue();
        let input = if matches!(queued.kind, InputKind::Compact) {
            TurnInput::Compact {
                command,
                config: queued.input.extras.config.clone(),
            }
        } else {
            let text = queued.input.text.clone();
            let extras = queued.input.extras.clone();
            TurnInput::Human {
                command,
                text,
                screened: queued.screened(),
                more: Vec::new(),
                extras: Box::new(extras),
            }
        };
        if let Some(running) = &self.running {
            running
                .abort
                .abort(rust_cli_kernel_api::abort::AbortReason::User);
            self.cancel_turn_asks();
            self.promoted = Some(input);
            return crate::actor::accepted();
        }
        match self.start_turn(input).await {
            Ok(()) => crate::actor::accepted(),
            Err(reason) => crate::actor::rejected(Refusal::Failed, reason),
        }
    }

    /// `Command::SubmitNow`（session-lifecycle.md §10.27，13 Q17）：回合在跑时（准入给 `Run`）在这一次处理里中止它、取消它等回答的询问、
    /// 把这条输入记成提升的输入，回合结束后 `start_next` 先开它；中止、记下不拆成两步，中间插不进别的输入或通知。别的情形（空闲、
    /// 后台工作在跑、扣住时要选择）同 `Submit`。
    pub(crate) async fn submit_now(
        &mut self,
        command: CommandId,
        text: String,
        extras: rust_cli_kernel_api::input_extras::InputExtras,
        admission: rust_cli_kernel_api::admission::Admission,
    ) -> rust_cli_kernel_api::session_api::Ack {
        let running = self
            .running
            .as_ref()
            .filter(|_| matches!(admission, rust_cli_kernel_api::admission::Admission::Run));
        let Some(running) = running else {
            return self.submit(command, text, extras, admission).await;
        };
        // 只记一条：同 `Promote`，后到的不覆盖先到的（Node 的前台提升租约冲突时同样拒绝）
        if self.promoted.is_some() || self.replacing.is_some() {
            return crate::actor::rejected(Refusal::Busy, PROMOTION_PENDING);
        }
        // `Submit` 而不是 `User`：照 Node 的「现在发」，不写中断标记、不把工具结果换成「用户停止」的文字，回合结束时也不扣住队列（turn.rs）
        running
            .abort
            .abort(rust_cli_kernel_api::abort::AbortReason::Submit);
        self.cancel_turn_asks();
        self.promoted = Some(TurnInput::Human {
            command,
            text,
            screened: None,
            more: Vec::new(),
            extras: Box::new(extras),
        });
        crate::actor::accepted()
    }

    /// `Command::SetFollowUp`（§10.27）：会话的跟随方式。设值语义：值没变不发事件。
    pub(crate) fn set_follow_up(&mut self, delivery: rust_cli_kernel_api::session_api::Delivery) {
        if self.follow_up == delivery {
            return;
        }
        self.follow_up = delivery;
        self.log
            .append(rust_cli_kernel_api::session_api::KernelEvent::FollowUpChanged { delivery });
    }

    /// 回合结束后的下一项（query.md §5）：通道头是输入就是它；是通知就取走头部连续的通知，合成一个
    /// 回合（通知合并）；通道空时是 `Queue` 输入（优先级 `later`）。定时任务由调用方最后看。
    pub(crate) fn next_turn_input(&mut self) -> Option<TurnInput> {
        // 排队的输入扣住时（session-lifecycle.md §10.6）只开后台通知的回合：扣住的是用户的输入。
        if self.queue_hold.is_some() {
            let notices = self.inbox.take_notices();
            return (!notices.is_empty()).then_some(TurnInput::Notices(notices));
        }
        // 只在工具边界并入的项（`-p` 的回执）不开回合：留在通道里，看后面的项。
        let passive = self.inbox.take_passive_prefix();
        let next = self.next_active_input();
        self.inbox.restore_front(passive);
        next
    }

    /// 通道头不是只在工具边界并入的项时的下一项。
    fn next_active_input(&mut self) -> Option<TurnInput> {
        match self.inbox.lane_front() {
            Some(LaneItem::Input(_)) => {
                let Some(Item::Input(queued)) = self.inbox.pop_lane_front() else {
                    return None;
                };
                self.emit_queue();
                Some(self.human_turn(queued))
            }
            Some(LaneItem::Notice(_)) => {
                let mut notices = Vec::new();
                while let Some(LaneItem::Notice(notice)) = self.inbox.lane_front() {
                    if notice.waiting.passive() {
                        break;
                    }
                    if let Some(Item::Notice(notice)) = self.inbox.pop_lane_front() {
                        notices.push(notice.waiting);
                    }
                }
                Some(TurnInput::Notices(notices))
            }
            None => {
                let next = self.inbox.pop_later()?;
                self.emit_queue();
                let command = next.input.command.clone();
                match next.kind {
                    InputKind::Bash(line) => Some(TurnInput::Bash { command, line }),
                    InputKind::Content(content) => Some(TurnInput::Content { command, content }),
                    InputKind::Compact => Some(TurnInput::Compact {
                        command,
                        config: next.input.extras.config,
                    }),
                    InputKind::Text => Some(self.human_turn(next)),
                }
            }
        }
    }

    /// 一条文字输入开的回合，连同与它成批的后续输入。
    fn human_turn(&mut self, queued: Queued) -> TurnInput {
        let more = self.take_batch(&queued);
        let command = queued.input.command.clone();
        let text = queued.input.text.clone();
        let extras = queued.input.extras.clone();
        let screened = queued.screened();
        TurnInput::Human {
            screened,
            command,
            text,
            more,
            extras: Box::new(extras),
        }
    }
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod batch_tests;
