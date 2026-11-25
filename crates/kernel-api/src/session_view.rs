//! [`SessionView`] 与它的构造（F126，单独一个文件：session_api.rs 接近行数上限；SMALL-6 合入 HOOK-ROWS 时把结构体也挪来）。

use serde::{Deserialize, Serialize};

use super::{Delivery, PlanSwitch, QueueHold, QueuedInput, Running};
use crate::ids::{InteractionId, MessageId, ShellTaskId};
use crate::input_extras::InputRecord;
use crate::message::Message;
use crate::model::ModelSelection;
use crate::orchestration::InteractionRequest;
use crate::permission::PermissionMode;
use crate::store::SessionMeta;

/// 会话的当前视图（前端重连或首次打开时用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub meta: SessionMeta,
    /// 显示用的历史：冷读取时是只供显示的压缩点之前的消息（[`crate::store::LoadedSession::earlier`]）接上会话 actor 的
    /// 历史（工作线 F102，docs/specs/rust-cli/compact.md §5.1）。要模型侧的那一段时取 [`crate::history::active`]。
    pub messages: Vec<Message>,
    pub selection: ModelSelection,
    /// 权限模式（修复线 F20 起不会是 `Plan`）。
    pub mode: PermissionMode,
    /// plan 开关（修复线 F20；serde 缺省 false）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    pub pending: Vec<(InteractionId, InteractionRequest)>,
    pub queue: Vec<QueuedInput>,
    pub last_seq: u64,
    /// `last_seq` 时的状态 revision（缺口 G1，同 [`SequencedEvent::revision`]；serde 缺省 0）。
    #[serde(default)]
    pub revision: u64,
    /// 在跑的是什么（session-lifecycle.md §10.13、§10.33、§10.35）：会话的回合带它的开始（回合中途取视图的前端据它指名停止、补建回合头），
    /// 子代理的快照只说子代理在跑；没有在跑的为 None。serde 缺省 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<Running>,
    /// 执行环境里还没有处理完的后台命令（`ShellState::background`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<ShellTaskId>,
    /// 在跑的后台工作（K-W1，docs/specs/rust-cli/session-lifecycle.md §10.28）：会话 actor 的后台工作表，按开始的先后；只在内存，
    /// 冷打开的为空。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub works: Vec<crate::background::BackgroundStart>,
    /// 扣住、等用户批准的同伴消息（cross-session.md §2.4，F28；serde 缺省为空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peer_held: Vec<crate::peers::HeldPeerMessage>,
    /// 上下文用量（token、有效窗口）：会话 actor 按当前历史与模型经 `Compactor::context_usage` 算（compact.md §2.1、
    /// tui-display.md §2.6，修复线 F27；子代理的快照由 agents 按子会话同样算，F112）；压缩模块不知道时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<crate::compaction::ContextOccupancy>,
    /// 上下文快满的提示（同一处算，F27）：冷加载后不用等下一条回复。
    #[serde(default)]
    pub context_notice: crate::compaction::ContextNotice,
    /// 排队的输入扣住了（§10.6）：只在内存里，冷打开的为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_hold: Option<QueueHold>,
    /// 会话的跟随方式（§10.27）：只在内存里，冷打开的是 `Queue`。
    #[serde(default, skip_serializing_if = "Delivery::is_queue")]
    pub follow_up: Delivery,
    /// 本进程里最近一个完成的主回合请求（子代理的快照是子代理请求，F112）的输出速度（F105）：只在内存里，冷打开的为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_speed: Option<crate::metering::OutputSpeed>,
    /// 回合记录（session-lifecycle.md §10.1）：打开时从库里读到的加上本进程里写的，按写入顺序。`messages` 里的每条消息
    /// 按它找回所属的回合。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<crate::store::TurnRecord>,
    /// 输入记录（session-lifecycle.md §10.14）：打开时从库里读到的加上本进程里写的，按写入顺序；键是用户消息的 id。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputRecord>,
    /// 输入的消息与它的命令（session-lifecycle.md §10.31）：打开时从库里读到的加上本进程里提交的，按写入顺序。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<crate::store::InputSource>,
    /// 工具结果的输出头部（session-lifecycle.md §10.34、§10.38）：打开时从库里读到的，加上本进程里结束的调用的（工具结束时与 `ToolDone`
    /// 同一次处理记下，结果随后同批落库），按记下的顺序。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_heads: Vec<crate::command_output::ToolOutputHead>,
    /// 在跑的回合里因停止而取消的调用（session-lifecycle.md §10.17、§10.38）：与 `ToolDone{cancelled}` 同一次处理记下；回合结束之后它们在
    /// 回合记录的 `ended.cancelled` 里，这里为空。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<crate::ids::ToolUseId>,
    /// 工具调用的开工与结束（session-lifecycle.md §10.37）：打开时从库里读到的加上本进程里提交的，按写入顺序。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_times: Vec<crate::metering::ToolTime>,
    /// ExitPlanMode 拿给用户批准的计划（session-lifecycle.md §10.43）：打开时从库里读到的加上本进程里结束的调用的（与 `ToolDone` 同一次处理）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plans: Vec<crate::plan::ShownPlan>,
    /// 文件改动被撤销过的回合的开回合输入（session-lifecycle.md §10.54）：打开时从库里读到的加上本进程里撤销的，按撤销的顺序、不重复。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reverted: Vec<MessageId>,
    /// 最近一次被接受的「放行并切模式」回答的询问的调用（session-lifecycle.md §10.25、§10.59）：只在内存，冷打开的为空。serde 缺省 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_grant: Option<crate::ids::ToolUseId>,
    /// 最近一次工具切的 plan（§10.25）：只在内存，冷打开的为空。serde 缺省 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_switch: Option<PlanSwitch>,
    /// 会话现在的附加工作目录（docs/specs/rust-cli/session-dirs.md §1）：只在内存，冷打开的是内核缺省。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<crate::env::EnvPath>,
    /// 会话级的权限规则层（docs/specs/rust-cli/permission-updates.md §3）：只在内存，冷打开的为空（`/permissions` 的列表读它）。
    /// serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_rules: Vec<crate::permission::SourcedRule>,
    /// 这个 actor 的寿命里记下的 hook 运行（session-lifecycle.md §10.51）：只在内存，冷打开的为空。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<crate::hook_runs::TurnHooks>,
    /// 这个 actor 的寿命里主线程派生的子代理（session-lifecycle.md §10.56），按开始的先后：只在内存，冷打开的为空（库里的关联是
    /// `StoreQueries::subagents`）。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<crate::subagent_runs::SubagentRun>,
    /// 会话偏好（docs/specs/rust-cli/app-prefs.md §4.2）：起始状态里的，打开着时不变。serde 缺省不关。
    #[serde(
        default,
        skip_serializing_if = "crate::session_preferences::SessionPreferences::is_default"
    )]
    pub preferences: crate::session_preferences::SessionPreferences,
    /// 这个会话自己的任务清单（agents.md §7.2，K-S9）：actor 与冷快照同一个读法（`AgentSignals::task_list`），子会话为空。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<crate::agents::TaskRecord>,
}

impl SessionView {
    /// 空闲、没有待回答的交互与排队输入的视图（`seq` 与 `revision` 为 0）；会话 actor 用结构更新填上运行时的状态。
    pub fn new(
        meta: SessionMeta,
        messages: Vec<Message>,
        selection: ModelSelection,
        mode: PermissionMode,
    ) -> Self {
        Self {
            meta,
            messages,
            selection,
            mode,
            plan: false,
            pending: Vec::new(),
            queue: Vec::new(),
            last_seq: 0,
            revision: 0,
            running: None,
            background: Vec::new(),
            works: Vec::new(),
            peer_held: Vec::new(),
            context_usage: None,
            context_notice: crate::compaction::ContextNotice::default(),
            output_speed: None,
            turns: Vec::new(),
            inputs: Vec::new(),
            sources: Vec::new(),
            output_heads: Vec::new(),
            cancelled: Vec::new(),
            tool_times: Vec::new(),
            plans: Vec::new(),
            reverted: Vec::new(),
            queue_hold: None,
            follow_up: crate::session_api::Delivery::Queue,
            mode_grant: None,
            plan_switch: None,
            additional_directories: Vec::new(),
            session_rules: Vec::new(),
            hooks: Vec::new(),
            subagents: Vec::new(),
            preferences: Default::default(),
            tasks: Vec::new(),
        }
    }

    /// 会话在用（归档会话的永久删除据它阻止，PD-D3，docs/specs/rust-cli/session-facts.md §5.1）：回合在跑、有待回答的交互、队列里有输入、
    /// 有在跑的后台工作。只看会话 actor 自己的状态（库里 Node 写的待处理工作另算）。
    pub fn in_use(&self) -> bool {
        self.running.is_some()
            || !self.pending.is_empty()
            || !self.queue.is_empty()
            || !self.background.is_empty()
            || !self.works.is_empty()
    }
}

/// 前端冷读只读最后一段的视图（[`super::Kernel::session_tail`]，docs/specs/rust-cli/node-store.md §5.6）：视图只带历史的最后一段时 `earlier` 为真。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTail {
    pub view: SessionView,
    /// 视图里主线程的 Agent 调用关联到的子会话（会话打开着时是库里的全部，同 `StoreQueries::subagents`）。
    pub subagents: Vec<crate::agents::StoredSubagent>,
    pub earlier: bool,
}
