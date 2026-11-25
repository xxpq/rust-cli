//! 输入调度（工作线 F130，architecture-review.md C6）：排队的输入、next 通道与它们的旁路状态收在一个结构里。
//!
//! 以前是 actor 的 4 个字段（`queue`、`lane`、`screened`、`queued_kinds`），在 7 个文件里直接修改；“通道里的输入一定在
//! 队列里”只靠约定，调度代码里留着找不到时的兜底分支。现在只有一个按到达顺序的序列 `items`：
//!
//! ```text
//!  items（到达顺序）： [输入 q1] [通知 n1] [插话 s1] [输入 q2] [通知 n2] …
//!  排队的输入（QueueChanged 与视图）： 全部输入，按顺序 → q1 s1 q2
//!  next 通道（优先级 next）：           通知与插话（还有 SessionStart 的首条输入），按顺序 → n1 s1 n2
//!  later（`Queue` 输入）：             其余输入，按顺序 → q1 q2
//! ```
//!
//! 一条输入只存一份，通道与队列都是它的投影，所以不变式由构造保证。输入的种类（文字、bash 模式、带内容块）与并入点
//! 筛查留下的结果跟着条目走。通道里的项与 later 输入之间的相对位置没有意义：通道里有项时总是先取通道。

use std::collections::VecDeque;

use rust_cli_kernel_api::hooks::HookOutcome;
use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::input_extras::TurnConfig;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Delivery, QueueEditLock, QueuedEdit, QueuedInput};

use crate::lane::Waiting;

/// 排队输入的种类（bash.rs，tui-input.md §2.2、§3.2）。
#[derive(Debug)]
pub(crate) enum InputKind {
    /// 普通文字（正文在 `QueuedInput.text`）。
    Text,
    /// bash 模式的命令原文。
    Bash(String),
    /// 带内容块的输入（粘贴的图片）。
    Content(Vec<ContentBlock>),
    /// 排队的压缩（v5 的 `session.compact`，docs/specs/rust-cli/session-lifecycle.md §10.23）：轮到它时开压缩的回合。
    Compact,
}

/// 并入点的筛查（steering.rs）。
#[derive(Debug)]
pub(crate) enum Screening {
    /// 还没在工具边界跑过 UserPromptSubmit。
    Unscreened,
    /// 跑过、留到回合结束后作为独立回合（屏障）：有结果时那个回合直接用它；None 时（领取后被中止）重跑 hook。
    Held(Option<Box<HookOutcome>>),
}

/// 一条排队的输入。
#[derive(Debug)]
pub(crate) struct Queued {
    pub(crate) input: QueuedInput,
    pub(crate) kind: InputKind,
    pub(crate) screening: Screening,
    /// 在 next 通道里：插话，与 SessionStart 给的首条输入；其余是 later。
    lane: bool,
    /// 自己一个回合，不与别的输入成批（SessionStart 给的首条输入，同对照实现的启动命令）。以前按命令 id 的前缀
    /// `session-start-` 认出它；命令 id 是 UUIDv7 之后不再在 id 里编码含义（session-lifecycle.md §10.2）。
    pub(crate) solo: bool,
}

impl Queued {
    /// 能与头一条成批进同一个回合（修复线 L-A，query.md §4.3）：普通文字、不是斜杠命令、不含 `@`、没有挂在开回合的输入上的
    /// 事实（session-lifecycle.md §10.14），执行配置与头一条相同（§10.25：成批了后一条的配置就丢了）。
    fn batches_with(&self, head: &TurnConfig) -> bool {
        // 锁着的项不出队（§10.59）：它也不并进别的回合，后面的项也就不越过它
        self.input.edit_lock.is_none()
            && !self.solo
            && !self.input.extras.own_turn()
            && self.input.extras.config == *head
            && matches!(self.kind, InputKind::Text)
            && !is_slash(&self.input.text)
            && !self.input.text.contains('@')
    }

    /// 套用前端改的配置（§10.59）：给了的项换掉，没给的保留。换了模型时这一项的执行覆盖与注记作废：它们是为原来的模型抽的卡
    /// （照 v4 的 `dropHighspeed`），带着它们开跑就用错了卡。
    fn configure(&mut self, config: &TurnConfig) {
        let extras = &mut self.input.extras;
        if let Some(selection) = &config.selection
            && extras.config.selection.as_ref() != Some(selection)
        {
            extras.config.selection = Some(selection.clone());
            extras.execution = None;
            extras.note = None;
        }
        if let Some(mode) = config.mode {
            extras.config.mode = Some(mode);
        }
        if let Some(plan) = config.plan {
            extras.config.plan = Some(plan);
        }
    }

    /// 在 next 通道里（插话、SessionStart 给的首条输入）：不能锁（§10.59，锁的是排在回合之后的输入）。
    pub(crate) fn in_lane(&self) -> bool {
        self.lane
    }

    /// 并入点筛查留下的结果（回合结束后开它时用，不再跑 hook）。
    pub(crate) fn screened(self) -> Option<Box<HookOutcome>> {
        match self.screening {
            Screening::Held(outcome) => outcome,
            Screening::Unscreened => None,
        }
    }
}

/// 通道里一条通知的身份：只在内存，工具边界领取与交回时用它认出是哪一条（工作线 F130，审查 C8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NoticeKey(u64);

/// 通道里的一条通知。
#[derive(Debug)]
pub(crate) struct LaneNotice {
    pub(crate) waiting: Waiting,
    pub(crate) key: NoticeKey,
    /// 在工具边界交给主循环时给它的 attachment 的消息 ID：那条消息提交之后按这个 ID 取走并 ack（lane.rs）。以前按
    /// attachment 的文字与通道里的通知全文相等找回。
    attachment: Option<MessageId>,
}

/// 序列里的一项。
#[derive(Debug)]
pub(crate) enum Item {
    Input(Queued),
    Notice(LaneNotice),
}

impl Item {
    /// 在 next 通道里（通知、插话）。
    fn in_lane(&self) -> bool {
        match self {
            Self::Input(queued) => queued.lane,
            Self::Notice(_) => true,
        }
    }
}

/// 通道里的一项（只读）。
pub(crate) enum LaneItem<'a> {
    Input(&'a Queued),
    Notice(&'a LaneNotice),
}

/// 斜杠命令：trim 之后以 `/` 开头（与插话的跳过规则相同，query.md §4）。
pub(crate) fn is_slash(text: &str) -> bool {
    text.trim().starts_with('/')
}

/// 会话 actor 的输入调度：只在 actor 里经 `&mut` 访问。
#[derive(Debug, Default)]
pub(crate) struct Inbox {
    items: VecDeque<Item>,
    /// 下一条通知的 [`NoticeKey`]。
    next_key: u64,
}

impl Inbox {
    /// 运行中到达的输入：插话（`Steer`）进通道，其余是 later。
    pub(crate) fn push_input(&mut self, input: QueuedInput, kind: InputKind) {
        let lane = input.delivery == Delivery::Steer;
        self.items.push_back(Item::Input(Queued {
            input,
            kind,
            screening: Screening::Unscreened,
            lane,
            solo: false,
        }));
    }

    /// 排队的压缩（§10.23）：later，排在全部项之后；不成批（不是普通文字）、不在工具边界并入（不在通道）。`config` 只有模型
    /// （§10.30）：轮到它时先写成会话的模型。
    pub(crate) fn push_compaction(&mut self, command: CommandId, config: TurnConfig) {
        self.items.push_back(Item::Input(Queued {
            input: QueuedInput {
                extras: rust_cli_kernel_api::input_extras::InputExtras {
                    config,
                    ..Default::default()
                },
                compact: true,
                ..QueuedInput::new(command, String::new(), Delivery::Queue)
            },
            kind: InputKind::Compact,
            screening: Screening::Unscreened,
            lane: false,
            solo: false,
        }));
    }

    /// 队列里有排队的压缩（§10.23 的 `compaction`）。
    pub(crate) fn has_compaction(&self) -> bool {
        self.inputs()
            .any(|queued| matches!(queued.kind, InputKind::Compact))
    }

    /// SessionStart 给的首条输入：排在最前（通道与队列的头部），回合结束后先开它（hooks.rs）。
    pub(crate) fn push_first(&mut self, input: QueuedInput) {
        self.items.push_front(Item::Input(Queued {
            input,
            kind: InputKind::Text,
            screening: Screening::Unscreened,
            lane: true,
            solo: true,
        }));
    }

    pub(crate) fn push_notice(&mut self, waiting: Waiting) {
        let key = NoticeKey(self.next_key);
        self.next_key += 1;
        self.items.push_back(Item::Notice(LaneNotice {
            waiting,
            key,
            attachment: None,
        }));
    }

    /// 排队的输入，按到达顺序（`QueueChanged` 与视图的唯一来源）。
    pub(crate) fn queued(&self) -> Vec<QueuedInput> {
        self.inputs().map(|queued| queued.input.clone()).collect()
    }

    fn inputs(&self) -> impl Iterator<Item = &Queued> {
        self.items.iter().filter_map(|item| match item {
            Item::Input(queued) => Some(queued),
            Item::Notice(_) => None,
        })
    }

    pub(crate) fn has_inputs(&self) -> bool {
        self.inputs().next().is_some()
    }

    /// 没有任何项（输入与通知）：会话空闲的判据之一（session-lifecycle.md §3.3.2）。
    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 通道里有插话（低延迟投递转不转后台，workspace.rs）。
    pub(crate) fn has_lane_input(&self) -> bool {
        self.inputs().any(|queued| queued.lane)
    }

    /// 通道里没有项（通知、插话）。
    pub(crate) fn lane_is_empty(&self) -> bool {
        !self.items.iter().any(Item::in_lane)
    }

    /// 通道里的项，按顺序（工具边界的领取，steering.rs）。
    pub(crate) fn lane(&self) -> impl Iterator<Item = LaneItem<'_>> {
        self.items.iter().filter_map(|item| match item {
            Item::Input(queued) if queued.lane => Some(LaneItem::Input(queued)),
            Item::Input(_) => None,
            Item::Notice(notice) => Some(LaneItem::Notice(notice)),
        })
    }

    /// 这条排队的项的种类（`Promote` 只提升文字与压缩，§10.13、§10.23；`EditQueued` 只改文字，§10.21）；不在队列里时为 None。
    pub(crate) fn kind_of(&self, command: &CommandId) -> Option<&InputKind> {
        self.inputs()
            .find(|queued| queued.input.command == *command)
            .map(|queued| &queued.kind)
    }

    /// 前端看得到的这一项（§10.21、§10.30）：整项，不含位置——位置不是项的字段，别的项出队、挪动不改它。
    pub(crate) fn queued_input(&self, command: &CommandId) -> Option<&QueuedInput> {
        self.inputs()
            .find(|queued| queued.input.command == *command)
            .map(|queued| &queued.input)
    }

    /// 在队列里的这一项（只读；改它经下面的几个方法）。
    pub(crate) fn entry(&self, command: &CommandId) -> Option<&Queued> {
        self.inputs()
            .find(|queued| queued.input.command == *command)
    }

    fn entry_mut(&mut self, command: &CommandId) -> Option<&mut Queued> {
        self.items.iter_mut().find_map(|item| match item {
            Item::Input(queued) if queued.input.command == *command => Some(queued),
            Item::Input(_) | Item::Notice(_) => None,
        })
    }

    /// 改一条排队的输入（§10.21、§10.59）：位置不变；并入点筛查的结果是对旧内容的结论，作废（下次重新筛）。给了的字段换掉，没给的保留；
    /// 配置的换法同 [`Queued::configure`]。
    pub(crate) fn edit(&mut self, command: &CommandId, edit: &QueuedEdit) {
        let Some(queued) = self.entry_mut(command) else {
            return;
        };
        queued.input.text.clone_from(&edit.text);
        if let Some(attachments) = &edit.attachments {
            queued.input.extras.files.clone_from(&attachments.files);
            queued.input.extras.uploads.clone_from(&attachments.uploads);
        }
        if let Some(quotes) = &edit.quotes {
            queued.input.extras.quotes.clone_from(quotes);
        }
        queued.configure(&edit.config);
        queued.screening = Screening::Unscreened;
    }

    /// 只换模型与权限模式（§10.59 的 `ApplyQueuedConfig`，plan 不动）。返回有没有换。
    pub(crate) fn reconfigure(&mut self, command: &CommandId, config: &TurnConfig) -> bool {
        let Some(queued) = self.entry_mut(command) else {
            return false;
        };
        let before = queued.input.extras.clone();
        queued.configure(&TurnConfig {
            selection: config.selection.clone(),
            mode: config.mode,
            plan: None,
        });
        queued.input.extras != before
    }

    /// 锁住或放开一条排队的项（§10.59）：锁是项的字段，项离开队列时锁一起没了。
    pub(crate) fn set_lock(&mut self, command: &CommandId, lock: Option<QueueEditLock>) {
        if let Some(queued) = self.entry_mut(command) {
            queued.input.edit_lock = lock;
        }
    }

    /// 第一条 later 输入被锁着（§10.59）：它不出队（[`Self::pop_later`]），准入的 `frozen`。
    pub(crate) fn later_head_locked(&self) -> bool {
        self.inputs()
            .find(|queued| !queued.lane)
            .is_some_and(|queued| queued.input.edit_lock.is_some())
    }

    /// 把一条排队的项挪到 `before` 之前（None：挪到最后，§10.21）。两者都在队列里由调用方保证。
    pub(crate) fn move_before(&mut self, command: &CommandId, before: Option<&CommandId>) {
        let Some(item) = self.remove_input(command).map(Item::Input) else {
            return;
        };
        let at = before.and_then(|before| {
            self.items.iter().position(
                |item| matches!(item, Item::Input(queued) if queued.input.command == *before),
            )
        });
        match at {
            Some(at) => self.items.insert(at, item),
            None => self.items.push_back(item),
        }
    }

    /// 去掉一条输入（并入、丢弃、成批取走）。
    pub(crate) fn remove_input(&mut self, command: &CommandId) -> Option<Queued> {
        let at = self.items.iter().position(
            |item| matches!(item, Item::Input(queued) if queued.input.command == *command),
        )?;
        match self.items.remove(at) {
            Some(Item::Input(queued)) => Some(queued),
            Some(Item::Notice(_)) | None => None,
        }
    }

    /// 「放行并切模式」（session-lifecycle.md §10.25、§10.59，照 v4）：带执行配置的输入的权限模式换成 `mode`，plan 不动；没带配置的开的时候
    /// 本来就跟会话。返回有没有改。
    ///
    /// 排队的压缩不改（§10.30）：压缩只取模型、不消费模式（照 v4）。它带了模型之后配置不再是空的，不跳过的话压缩开跑时会把会话的
    /// 模式写回授权时的模式（用户在这之间切走过的话）。
    pub(crate) fn grant_mode(&mut self, mode: PermissionMode) -> bool {
        let mut changed = false;
        for item in &mut self.items {
            if let Item::Input(queued) = item
                && !matches!(queued.kind, InputKind::Compact)
                && !queued.input.extras.config.is_unset()
            {
                queued.input.extras.config.mode = Some(mode);
                changed = true;
            }
        }
        changed
    }

    /// 并入点筛查后留下（`Fate::Waits`）。
    pub(crate) fn hold(&mut self, command: &CommandId, outcome: Option<Box<HookOutcome>>) {
        for item in &mut self.items {
            if let Item::Input(queued) = item
                && queued.input.command == *command
            {
                queued.screening = Screening::Held(outcome);
                return;
            }
        }
    }

    /// 通道里有没有同一条通知（重复送达）。
    pub(crate) fn has_notice(&self, notice: &Waiting) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, Item::Notice(queued) if queued.waiting.same(notice)))
    }

    /// 工具边界把这条通知交给主循环，它的 attachment 用消息 ID `attachment`（再领取时换成新的）。
    pub(crate) fn absorbing(&mut self, key: NoticeKey, attachment: MessageId) {
        for item in &mut self.items {
            if let Item::Notice(notice) = item
                && notice.key == key
            {
                notice.attachment = Some(attachment);
                return;
            }
        }
    }

    /// 在工具边界交给主循环、attachment 用消息 ID `attachment` 的那一项（还在通道里）：提交这条 attachment 时把它并入的
    /// 事实放进同一批（append.rs，F139）。
    pub(crate) fn absorbing_item(&self, attachment: &MessageId) -> Option<&Waiting> {
        self.items.iter().find_map(|item| match item {
            Item::Notice(notice) if notice.attachment.as_ref() == Some(attachment) => {
                Some(&notice.waiting)
            }
            _ => None,
        })
    }

    /// 消息 ID 是 `attachment` 的那条通知已经并入对话：取走它。
    pub(crate) fn take_absorbed(&mut self, attachment: &MessageId) -> Option<Waiting> {
        let at = self.items.iter().position(|item| {
            matches!(item, Item::Notice(notice) if notice.attachment.as_ref() == Some(attachment))
        })?;
        match self.items.remove(at) {
            Some(Item::Notice(notice)) => Some(notice.waiting),
            Some(Item::Input(_)) | None => None,
        }
    }

    /// 通道头部连续的、只在工具边界并入的通知（`-p` 的回执，passive）：开回合时跳过它们，之后用
    /// [`Self::restore_front`] 放回原处。
    pub(crate) fn take_passive_prefix(&mut self) -> Vec<Item> {
        let mut taken = Vec::new();
        while let Some(at) = self.items.iter().position(Item::in_lane) {
            let passive =
                matches!(&self.items[at], Item::Notice(notice) if notice.waiting.passive());
            if !passive {
                break;
            }
            if let Some(item) = self.items.remove(at) {
                taken.push(item);
            }
        }
        taken
    }

    /// 放回 [`Self::take_passive_prefix`] 取走的项（仍在通道的最前面）。
    pub(crate) fn restore_front(&mut self, items: Vec<Item>) {
        for item in items.into_iter().rev() {
            self.items.push_front(item);
        }
    }

    /// 通道里全部不只在工具边界并入的通知，按顺序取走（队列扣住时只开通知的回合，session-lifecycle.md §10.6）。
    pub(crate) fn take_notices(&mut self) -> Vec<Waiting> {
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            match item {
                Item::Notice(notice) if !notice.waiting.passive() => taken.push(notice.waiting),
                other => kept.push_back(other),
            }
        }
        self.items = kept;
        taken
    }

    /// 通道里 `fenced` 的工作的终态通知，取走（§10.55：改写历史截掉了起它们的调用，不进新分支）。
    pub(crate) fn take_notices_of(&mut self, fenced: impl Fn(&str) -> bool) -> Vec<Waiting> {
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            match item {
                Item::Notice(notice) if notice.waiting.work_id().is_some_and(|id| fenced(&id)) => {
                    taken.push(notice.waiting);
                }
                other => kept.push_back(other),
            }
        }
        self.items = kept;
        taken
    }

    /// 通道头上的项是什么（不取走）。
    pub(crate) fn lane_front(&self) -> Option<LaneItem<'_>> {
        self.lane().next()
    }

    /// 取走通道头上的项。
    pub(crate) fn pop_lane_front(&mut self) -> Option<Item> {
        let at = self.items.iter().position(Item::in_lane)?;
        self.items.remove(at)
    }

    /// 取走第一条 later 输入（通道为空时，优先级 `later`）。它被编辑器锁着时不取（§10.59）：它与它后面的 later 输入都等着，取下一项的唯一
    /// 入口在这里，锁着的队首不会被开出去。
    pub(crate) fn pop_later(&mut self) -> Option<Queued> {
        let at = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Input(queued) if !queued.lane))?;
        if matches!(&self.items[at], Item::Input(queued) if queued.input.edit_lock.is_some()) {
            return None;
        }
        match self.items.remove(at) {
            Some(Item::Input(queued)) => Some(queued),
            Some(Item::Notice(_)) | None => None,
        }
    }

    /// 与头一条成批的下一条（修复线 L-A，query.md §4.3）：通道头是插话时是它，是通知时没有；通道空时是第一条 later
    /// 输入。它能与执行配置是 `head` 的头一条成批（[`Queued::batches_with`]）时取走。
    pub(crate) fn take_batched(&mut self, head: &TurnConfig) -> Option<Queued> {
        let at = match self.items.iter().position(Item::in_lane) {
            Some(at) => match &self.items[at] {
                Item::Input(_) => at,
                Item::Notice(_) => return None,
            },
            None => self
                .items
                .iter()
                .position(|item| matches!(item, Item::Input(_)))?,
        };
        match &self.items[at] {
            Item::Input(queued) if queued.batches_with(head) => {}
            Item::Input(_) | Item::Notice(_) => return None,
        }
        match self.items.remove(at) {
            Some(Item::Input(queued)) => Some(queued),
            Some(Item::Notice(_)) | None => None,
        }
    }
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
