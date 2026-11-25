//! 会话命令（前端发给会话 actor 的命令、投递方式与回退范围）；从 `session_api.rs` 拆出（800 行上限，REWIND 合入时），经 `session_api`
//! 重新导出，路径不变。

use serde::{Deserialize, Serialize};

use crate::ids::{CommandId, InteractionId, MessageId, SessionId};
use crate::input_extras::{AttachedFile, InputExtras, Quote, TurnConfig, UploadedFile};
use crate::model::ModelSelection;
use crate::permission::PermissionMode;

use super::QueuedInput;

/// 运行中提交的输入怎样投递（工具边界吸收排队消息）。也是会话的跟随方式（[`Command::SetFollowUp`]，缺省 `Queue`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// 当前回合结束后作为新回合发送。
    #[default]
    Queue,
    /// 在下一个工具边界注入当前回合。
    Steer,
}

impl Delivery {
    /// 缺省值（serde 不写出）。
    pub fn is_queue(&self) -> bool {
        *self == Self::Queue
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewindScope {
    Conversation,
    Code,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Submit {
        text: String,
        /// 正文之外的事实（docs/specs/rust-cli/session-lifecycle.md §10.14）：不为空时这条输入自己开一轮、运行中一律排队。
        #[serde(default, skip_serializing_if = "InputExtras::is_empty")]
        extras: InputExtras,
        delivery: Delivery,
        /// 插话指名的回合（docs/specs/rust-cli/session-lifecycle.md §10.5，v5 的 guide）：在跑的就是它时在工具边界并入；
        /// 没有回合在跑时按普通输入；在跑的是别的回合时排队，绝不并入别的回合。省略时是以前的行为。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<crate::ids::TurnId>,
    },
    /// 停止（§10.5）：`target` 是前端看到的在跑的回合；在跑的不是它时不打断（`Rejected{StopTargetChanged}`），没有回合在跑时
    /// `Accepted{Unchanged}`。省略时是 TUI 的 Esc：中止在跑的回合或 SessionStart，并取消本会话的 loop 唤醒。
    Interrupt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<crate::ids::TurnId>,
    },
    /// 进程要退出（修复线 F38，docs/specs/rust-cli/session-lifecycle.md §3.5）：以 [`crate::abort::AbortReason::Shutdown`]
    /// 中止在跑的回合（之后这个回合不再记录），此后不再开回合。没有回合在跑时也 `Accepted`。
    Shutdown,
    /// esc 撤回刚发的输入（修复线 F38，session-lifecycle.md §5.5）：空闲、队列为空、
    /// 最后一条可选的人类输入之后没有回答时，对话回退到它之前（与 `Rewind{conversation}` 同一个 `session.revert`）。
    /// 回执是 [`CommandResult::Restored`]（这条输入，前端放回输入框）。条件不满足时 `Rejected`。
    RestoreCancelled,
    ResolveInteraction {
        interaction: InteractionId,
        answer: crate::interaction::Answer,
    },
    SetModel {
        selection: ModelSelection,
    },
    /// 改权限模式（plan 开关不变）；`Plan` 是旧写法，只打开 plan（修复线 F20，shell-permissions.md §4.6）。
    SetMode {
        mode: PermissionMode,
    },
    /// 打开或关掉 plan 开关，权限模式不变（修复线 F20，TUI 的 `/plan`；Node `setExecutionState({planEnabled})`）。
    SetPlan {
        enabled: bool,
    },
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    Rewind {
        target: MessageId,
        scope: RewindScope,
    },
    /// 换掉一条输入（docs/specs/rust-cli/session-lifecycle.md §10.53，v5 的编辑重发与重试）：对话回退到 `target` 之前（`restore_code` 时先恢复
    /// target 起改过的文件，同 [`Command::Rewind`] 的 `Both`），再以这条命令开一个人类输入的回合（输入的 id 就是命令的 id）。空闲时在一次处理里
    /// 做完；回合在跑时同停止一样中止它，回合结束后先做这一条（前端拼 `Rewind` 与 `Submit` 时，两步之间可能开了别的回合）。回执在新回合开了之后：
    /// [`CommandResult::Rewound`]；文件恢复不了时 `Rejected{Unavailable}`，对话与文件都不动。
    ReplaceInput {
        target: MessageId,
        text: String,
        #[serde(default, skip_serializing_if = "InputExtras::is_empty")]
        extras: InputExtras,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        restore_code: bool,
        /// 换之前先停被截掉的回合里起的后台工作（docs/specs/rust-cli/session-lifecycle.md §10.55）：会话 actor 按自己的记录定出它们，与截断
        /// 同一步停掉；它们之后迟到的终态通知 ack 了丢掉，不进新分支。缺省不停（与 `/rewind` 相同）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stop_cut_works: bool,
    },
    /// 从 `at` 处分叉出新会话；回执是 [`CommandResult::Forked`]。
    Fork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<MessageId>,
    },
    /// 用户改名（`/rename`，Node `setCustomSessionTitle`）：标题来源记为 `custom`，之后不再自动生成（docs/specs/rust-cli/titles.md §4）。
    Rename {
        title: String,
    },
    /// 同 [`Command::Rename`]，另带这次命令的记录（TUI 的 `/rename <名字>`，docs/specs/rust-cli/titles.md §4）：空闲时记录与
    /// 改名同一批提交，改名被拒绝时记录也不在；回合或压缩在跑时改名马上提交，记录在改名成功之后留到这次工作收尾
    /// （同 [`Command::RecordLocalCommand`]）。
    RenameRecorded {
        title: String,
        record: crate::user_command::LocalCommandRecord,
    },
    /// 不带参数的 `/rename`（docs/specs/rust-cli/titles.md §8）：会话 actor
    /// 用模型从对话起名，再按 `Rename` 写入。回执等名字：[`CommandResult::Renamed`]，
    /// 起不出名字时 `Rejected.reason` 是给用户看的文本。
    GenerateName,
    /// 带内容块的输入（文字加粘贴的图片，docs/specs/rust-cli/tui-input.md §3）：回合与 [`Command::Submit`]
    /// 相同，输入消息的内容就是这些块；UPS、标题与斜杠展开只看文字块。
    SubmitContent {
        content: Vec<crate::message::ContentBlock>,
        delivery: Delivery,
    },
    /// bash 模式的输入（`!命令`，tui-input.md §2）：会话经 [`crate::user_input::UserInputs::bash`] 执行，
    /// 记下 `<bash-input>` 与输出两条 user 消息，再调模型。
    Bash {
        command: String,
        delivery: Delivery,
    },
    /// `/fork`（docs/specs/rust-cli/session-lifecycle.md §6.5，工作线 F75）：从一条可选的用户输入分出新会话，
    /// 新会话带到这一轮为止（这一轮最后一条 assistant），文件不动。只在空闲时接受；回执是 [`CommandResult::Forked`]
    /// （`through` 是复制到的那条消息，F78）。`input` 不是可选的输入时 `Rejected`。
    ForkFromInput {
        input: MessageId,
    },
    /// v5 的 fork（`turn.fork`，docs/specs/rust-cli/session-lifecycle.md §10.57）：同 [`Command::ForkFromInput`]，新会话的 id 是 `session`（调用方给，
    /// 02 §11.2 F：它已经是从这个会话分出的 fork 时什么都不写，回同一个结果；是别的会话时 `Rejected{Unavailable}`）。回合在跑时也接受（不打断它），
    /// 只要 `input` 这一轮已经结束；压缩与会话的后台工作在跑时 `Rejected{Busy}`。
    ForkFromInputAs {
        input: MessageId,
        session: SessionId,
    },
    /// v5 的侧聊（`session.create{kind: sideChat}`，docs/specs/rust-cli/session-lifecycle.md §10.58）：从这个会话最近一次稳定落盘的边界分出隐藏的子会话，
    /// id 是 `session`（调用方给；已经是从这个会话分出的侧聊时什么都不写，回同一个结果；是别的会话时 `Rejected{Unavailable}`）。只读已提交的记录、
    /// 不改这个会话，所以回合、压缩与会话的后台工作在跑时都接受。回执是 [`CommandResult::Forked`]（`through` 为 None）。
    SideChatAs {
        session: SessionId,
    },
    /// 批准或拒绝一条扣住的同伴消息（docs/specs/rust-cli/cross-session.md §2.4，F28）：`message` 是
    /// [`crate::peers::HeldPeerMessage::msg_id`]。已经解决（过期、改模式放行）时 `Rejected`。
    ResolvePeerMessage {
        message: String,
        approve: bool,
    },
    /// 停止会话里的一个后台任务（`/tasks`，docs/specs/rust-cli/tui-display.md §3.1）：与 TaskStop 工具同一条
    /// 停止路径。`task` 是 [`crate::manage::BackgroundEntry::id`]；回执是 [`CommandResult::Stopped`]，
    /// 找不到或不在跑时 `Rejected.reason` 是给用户看的文本。
    StopTask {
        task: String,
    },
    /// `/cd` 由会话 actor 判定（docs/specs/rust-cli/tui-manage.md §7.1，F1）：运行中 `Rejected`（固定的文字）；
    /// 否则回执是 [`CommandResult::Directory`]。移动时 actor 设 shell cwd、改工作区、
    /// 发 [`KernelEvent::WorkspaceChanged`]，给模型的说明（固定的说明文字加新目录的规则文件）由 actor 组成并提交。
    ChangeDirectory {
        root: crate::env::EnvPath,
    },
    /// 同 [`Command::ChangeDirectory`]，移动成功时 `record` 与说明文字同一批、排在它之前提交（修复线 L-A，
    /// docs/specs/rust-cli/tui-manage.md §11.2：对照实现的 `/cd` 先记命令输入与输出，说明文字是之后的 meta 消息）。
    ChangeDirectoryRecorded {
        root: crate::env::EnvPath,
        record: crate::user_command::LocalCommandRecord,
    },
    /// `/add-dir`（docs/specs/rust-cli/session-dirs.md §3.2）：`directory` 是 runtime 检查过的绝对路径。actor 只做纯判定：已在
    /// 某个工作目录里时不改；否则追加、发 [`KernelEvent::DirectoryAdded`]。两种结局都按 `args`（用户给的原文）记进对话
    /// （回合在跑时留到收尾）。回执是 [`CommandResult::DirectoryAddition`]。
    AddDirectory {
        directory: crate::env::EnvPath,
        args: String,
        /// 确认框里选了「记住」：加进去时另提交 `Fact::DirectoriesRemembered`，存不上仍加进会话（permission-updates.md §6.1）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        remember: bool,
    },
    /// 一组权限更新（docs/specs/rust-cli/permission-updates.md §4）：会话 actor 一批提交、成功后改会话层、模式与目录。前端用它删
    /// `/permissions` 列出的会话层规则。回执 `Accepted{Done}`；落库失败是 `Rejected{Failed}`。
    UpdatePermissions {
        updates: Vec<crate::permission_updates::PermissionUpdate>,
    },
    /// 本地命令写进对话的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11）：空闲时马上提交一批；回合或压缩在跑时
    /// 留到它收尾、开下一个回合之前提交。总是 `Accepted`（提交失败只记日志）。
    RecordLocalCommand {
        record: crate::user_command::LocalCommandRecord,
    },
    /// 把这个会话在跑的前台 Bash 转到后台（修复线 L-A，docs/specs/rust-cli/tui-input.md §11）：用户的转后台键
    /// （`User`），或低延迟投递的输入（`DeliverMessage`）。会话 actor 交给本机的执行环境；总是 `Accepted`：没有回合在跑
    /// 时是 [`CommandResult::Done`]，否则是 [`CommandResult::Backgrounded`]（转了的个数）。
    BackgroundForeground {
        reason: crate::env_command::BackgroundReason,
    },
    /// 把续写时补在内存里、还没落库的 assistant（`No response requested.`，docs/specs/rust-cli/session-lifecycle.md
    /// §6.3.1，工作线 F79；有补给未结调用的中断结果时一起，§6.3，工作线 F81）现在单独提交一批。交互界面在会话里切到这个会话（`/resume`、`/fork`）之后发：切过去就算换了
    /// 对话。没有要写的（没有补、已经写过、被回退掉）时什么都不做，回执是 `Accepted`；提交失败时 `Rejected`，它留到下一批
    /// 带消息的提交。
    StoreResumeSentinel,
    /// 取回还在排队的输入（query.md §4.4）：`command` 是提交它的 [`CommandEnvelope::id`]。在队列里时出队（`QueueChanged`）、`Accepted`；已经发出（开了回合或并入了回合）时 `Rejected`，`reason` 给用户看。
    Dequeue {
        command: CommandId,
    },
    /// 排队的输入是否在回合结束后自动开下一条（§10.6）：`true` 放开扣住的队列（空闲就开下一条），`false` 扣住。
    SetAutoDrain {
        enabled: bool,
    },
    /// 给一条输入的注记补字段（docs/specs/rust-cli/session-lifecycle.md §10.48）：`message` 在会话的历史里、它的输入记录带注记时，`patch` 的键浅合并进去、
    /// 同一条输入记录再写一次，发 `InputRecorded`。消息不在 `Rejected{NotFound}`；没有注记 `Rejected{Unavailable}`。回合在跑时也接受。
    PatchNote {
        message: MessageId,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    /// 现在开这一条输入，排队的输入继续扣住（docs/specs/rust-cli/session-lifecycle.md §10.13，v5 的 `keepQueue`）：扣住时
    /// [`Command::Submit`] 被拒（`QueueChoiceRequired`），放开扣住又会先开队头。只在空闲时接受。
    SubmitKeepingQueue {
        text: String,
        #[serde(default, skip_serializing_if = "InputExtras::is_empty")]
        extras: InputExtras,
        /// 前端看到的扣住的队列（§10.16，v5 的 `expectedHeldQueueItemIds`）：队列扣住而现在的输入集合与它不同时
        /// `Rejected{HeldQueueChanged}`，什么都不做。省略时不查。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_held: Option<Vec<CommandId>>,
    },
    /// 取空扣住的队列，再按普通输入发这一条（docs/specs/rust-cli/session-lifecycle.md §10.16，v5 的 `discardQueue`）：取空与提交
    /// 在 actor 的一次处理里，中间插不进别的命令与回合。队列没有扣住时不取，只发这一条。`expected_held` 同 `SubmitKeepingQueue`。
    SubmitDiscardingQueue {
        text: String,
        #[serde(default, skip_serializing_if = "InputExtras::is_empty")]
        extras: InputExtras,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_held: Option<Vec<CommandId>>,
    },
    /// 把一条排队的输入现在开成自己的一轮（§10.13，v5 的 `queue.sendNow`）：`command` 是提交它的命令 id；`target` 是前端
    /// 看到的在跑的回合。指名的回合在跑时中止它、回合结束后先开这一条（不论扣住）；空闲时立即开；在跑的是别的回合（或
    /// 没指名却有回合在跑）时 `Rejected{StopTargetChanged}`。
    Promote {
        command: CommandId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<crate::ids::TurnId>,
    },
    /// 改一条排队的文字输入，位置不变（docs/specs/rust-cli/session-lifecycle.md §10.21、§10.30、§10.59，v5 的 `queue.edit`）：`edit` 里给了的字段换掉，
    /// 没给的保留。前置条件是 `guard`（[`EditGuard`]）。不在队列里时 `Rejected{NotFound}`；不是普通文字（压缩、bash 模式、带内容块）时 `Rejected{Unavailable}`。
    EditQueued {
        command: CommandId,
        edit: Box<QueuedEdit>,
        guard: EditGuard,
    },
    /// 锁住一条排队的文字输入（§10.59，v5 的 `queue.lockEdit`）：锁着的项不出队、不被取回、挪动或提升。不在队列里 `Rejected{NotFound}`；不是 later 里的
    /// 普通文字 `Rejected{Unavailable}`；别的编辑器锁着 `Rejected{QueueItemLocked}`；同一个编辑器锁着时 `Accepted`、不改状态。
    LockQueued {
        command: CommandId,
        editor: String,
    },
    /// 放开一条排队项的锁（§10.59，v5 的 `queue.unlockEdit`）：谁都能放开。不在队列里、没有锁时 `Accepted{Unchanged}`；放开之后空闲就开下一项。
    UnlockQueued {
        command: CommandId,
    },
    /// 把模型与（或）权限模式套到这几条排队的文字输入上（§10.59，v5 的 `queue.applyConfig`）：`config` 只看 `selection` 与 `mode`（plan 不动）；
    /// 不在、锁着、不是普通文字的跳过。一项都没换时 `Accepted{Unchanged}`。
    ApplyQueuedConfig {
        commands: Vec<CommandId>,
        config: TurnConfig,
    },
    /// 把一条排队的项挪到 `before` 之前，`before` 为 None 时挪到最后（§10.21、§10.30，v5 的 `queue.reorder`）：只改顺序，不带前置的内容。
    /// 它不在队列里、或 `before` 不在队列里时 `Rejected{NotFound}`；`before` 是它自己时不变。
    MoveQueued {
        command: CommandId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<CommandId>,
    },
    /// 回答一个权限询问为「放行并切模式」（§10.22、§10.59，v5 的 `fullAccess` 与 `allowAndSwitchGuarded`）：权限模式还不是 `mode` 时先切过去（只改这个会话），
    /// 再按允许这一次回答。两步在 actor 的一次处理里：询问不在时什么都不改（`NotFound`）；切模式落库失败时询问留着（`Failed`）。
    ResolveSwitchingMode {
        interaction: InteractionId,
        mode: PermissionMode,
    },
    /// 压缩，回合在跑或队列扣住时排进队列的最后（§10.23，v5 的 `session.compact`，02 R21）：空闲时同 [`Command::Compact`]（没有压缩
    /// 说明）。已经有在跑、提升了还没开、或排队的压缩时 `Rejected{CompactLocked}`。`selection` 是这一次提交的模型（§10.30，照 v4）：
    /// 压缩开跑时先写成会话的模型；None 时用开跑那一刻会话的模型。
    CompactOrQueue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selection: Option<Box<ModelSelection>>,
    },
    /// 改会话的跟随方式（docs/specs/rust-cli/session-lifecycle.md §10.27，K-S4）：[`Command::SubmitFollowUp`] 在回合在跑时怎样投递。
    /// 只在内存；值没变时什么都不发。
    SetFollowUp {
        delivery: Delivery,
    },
    /// 一条按会话的跟随方式投递的输入（§10.27）：回合在跑时跟随方式是 `Steer` 就并入这一轮，否则排队；空闲时同 [`Command::Submit`]。
    SubmitFollowUp {
        text: String,
        #[serde(default, skip_serializing_if = "InputExtras::is_empty")]
        extras: InputExtras,
    },
    /// 现在发（§10.27，13 Q17）：回合在跑时在同一步里中止它（[`crate::abort::AbortReason::Submit`]：不写中断标记，队列不因此扣住），
    /// 回合结束后先开这一条；已有提升了还没开的输入时 `Rejected{Busy}`。空闲时同 [`Command::Submit`]；会话的后台工作在跑时排队。
    SubmitNow {
        text: String,
        #[serde(default, skip_serializing_if = "InputExtras::is_empty")]
        extras: InputExtras,
    },
}

/// 带 ID 的命令；同一个 ID 重复提交时返回 [`Ack::Duplicate`]（带原来的回执），不重复执行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub id: CommandId,
    pub command: Command,
}

/// 改一条排队的输入（§10.59）：`text` 总是换；其余给了（`Some`）的换掉，没给的保留。`config` 的每一项同理（`None` = 保留）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedEdit {
    pub text: String,
    /// 附件整组换掉（本机文件与上传是同一组附件的两种，一起给）：`Some` 里两组都为空 = 清空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<QueuedAttachments>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotes: Option<Vec<Quote>>,
    #[serde(default, skip_serializing_if = "TurnConfig::is_unset")]
    pub config: TurnConfig,
}

impl QueuedEdit {
    /// 只换正文。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: None,
            quotes: None,
            config: TurnConfig::default(),
        }
    }
}

/// 一条输入的附件（[`QueuedEdit::attachments`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedAttachments {
    pub files: Vec<AttachedFile>,
    pub uploads: Vec<UploadedFile>,
}

/// [`Command::EditQueued`] 的前置条件（§10.30、§10.59）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditGuard {
    /// 前端看到的这一项（整项；位置不是项的字段）：现在的项与它不同时 `Rejected{Stale}`；项锁着时 `Rejected{QueueItemLocked}`。
    Seen { expected: Box<QueuedInput> },
    /// 持锁的编辑器（Composer 编辑态）：锁不是它的时 `Rejected{EditLockLost}`；改完同一步放开锁。不比内容：锁住之后只有它改这一项。
    Lock { editor: String },
}
