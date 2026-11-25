//! 会话的投影（02 §1、§2、§5；protocol-v5.md §15.3）：输入是内核的 `SessionView` 与 `SequencedEvent`，输出是 4 种 op 的一批。
//! 前端里的纯逻辑：连接的 actor 拿着它与 topic 的 Publisher（内容只存在 Publisher 里，投影只记推导下一批要的几样）。
//!
//! 冷读与活会话走同一组函数（`rows::parts`、`Edit` 的行写法），所以同一段对话两边是同一组行（`projection_tests`）。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rust_cli_kernel_api::agents::TaskRecord;
use rust_cli_kernel_api::command_output::CommandOutputHead;
use rust_cli_kernel_api::compaction::ContextOccupancy;
use rust_cli_kernel_api::ids::{CommandId, InteractionId, MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{BlockDelta, ModelNotice, ModelSelection};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Delivery, DropReason, InputEnd, InputFate, KernelEvent, PlanSwitch, QueueHold, QueuedInput,
    SequencedEvent, SessionView, TurnOrigin, TurnOutcome, TurnStart,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use serde_json::{Value, json};

use super::inputs;
use super::interactions::{self, INTERACTIONS};
use super::live;
use super::preview;
use super::rows::{
    self, CREATED_AT_SEQ, DONE, Part, ROWS, STREAMING, keys, kinds, tool_status, turn_state,
};
use super::state::{self, ControlFacts, Outcome, Phase, Running};
use crate::consts::{INPUT_FATE_RETAIN, ROW_NUMBER_ANCHOR, SESSION_SNAPSHOT_ROWS};
use crate::generated::contract::codes;
use crate::publisher::{Batch, Content, Fields, Publisher};
use edit::Edit;
use rust_cli_kernel_api::turn_walk::{Opened, TurnWalk};

/// 队列与输入的集合（02 §1）。
pub(crate) const QUEUE: &str = "queue";
pub(crate) const INPUTS: &str = "inputs";
pub(crate) const WORKS: &str = "works";

/// 投影建起来时要的、视图之外的事实（protocol-v5.md §15.2）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Origin {
    /// 通道里会话打开着（视图来自内核的会话 actor）；否则是冷读。
    pub(crate) live: bool,
    /// 本进程里以 deferred 建、还没有回合的草稿。
    pub(crate) draft: bool,
    pub(crate) deleted: bool,
    pub(crate) title_source: TitleSource,
    /// `state.mcpAdmission`（18 §12.5）：通道打开会话时读一次信任记录算出；冷读为 null。
    pub(crate) mcp: Value,
    /// 库里的子代理（`StoreQueries::subagents`，§54.3）：通道在读视图时一起读；与视图的 `subagents` 合并成子代理行。
    pub(crate) subagents: Vec<rust_cli_kernel_api::agents::StoredSubagent>,
    /// 冷读在读快照之前读到的库里的戳（会话事实与分支切点，§70）：投影由之后读的快照建，所以与库里这一份相同。活视图与改写历史之后为 None
    /// （投影跟着内核的事件走，不再是库里某一份的样子）。
    pub(crate) stamp: Option<super::retain::Stamp>,
    /// 从历史的哪里建（§73.1）：整段、冷读的最后一段、激活时沿用原来的下沿。
    pub(crate) start: Start,
}

/// 投影从历史的哪里建（protocol-v5.md §73.1）：决定第一行的号与「下沿之前还有没有」。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Start {
    /// 视图就是整段历史：行号从 0 起。
    #[default]
    Whole,
    /// 视图只是最后一段、之前还有（冷读，§73.2）：行号从 [`ROW_NUMBER_ANCHOR`] 起，往前补的段往前编。
    Tail,
    /// 激活（冷 → 热）：活视图里这条存储消息之前的不建，行号从 `number` 起（原来最早一行的号），所以与冷的那份逐号相同（§73.0 第 1 条）。
    Floor { record: MessageId, number: u64 },
}

/// 一条 assistant 消息在流式中建过的行。
#[derive(Debug, Default)]
struct Stream {
    turn: String,
    rows: BTreeSet<String>,
    /// 思考块：模型的块序号 → 这条消息里第几个思考行。
    reasoning: BTreeMap<usize, usize>,
}

/// 会话的投影：推导下一批要的事实。内容在 Publisher 里。
#[derive(Debug)]
pub(crate) struct Projection {
    /// 已应用到的内核 seq：`seq ≤ applied` 的事件丢掉（物化时是视图的 `last_seq`，§15.2）。
    pub(crate) applied: u64,
    live: bool,
    draft: bool,
    deleted: bool,
    running: Option<Running>,
    outcome: Option<Outcome>,
    error: Option<Value>,
    retry: Option<Value>,
    hold: Option<QueueHold>,
    title: String,
    title_source: TitleSource,
    /// 侧聊会话（视图的 `meta.kind`，§57.5）：`meta.kind`、门与哪些消息成行都看它。
    side_chat: bool,
    /// 当前的 `mcpAdmission`（18 §12.6）：建投影时来自 [`Origin`]，本进程的 `mcp.trust.set` 成功之后换成写入之后的值。
    mcp: Value,
    created_at: u64,
    selection: ModelSelection,
    mode: PermissionMode,
    plan: bool,
    /// 内核的 `full_access` 与 `plan_switch`（§18.2）：进 `config` 的两个事实。
    grant: Option<ToolUseId>,
    plan_switch: Option<PlanSwitch>,
    /// 内核的会话跟随方式（§22.1）：`config.followupMode` 与 `admission.input` 都看它。
    follow_up: Delivery,
    context: Option<ContextOccupancy>,
    /// 行的全序 `(createdAtSeq, id)` 与下一个号（一行一个、不重用，§15.3.4）。
    order: BTreeSet<(u64, String)>,
    next_row: u64,
    streams: BTreeMap<MessageId, Stream>,
    /// 还没有结局的工具行 → 它的回合。
    open_tools: BTreeMap<String, String>,
    /// 运行中的工具的输出尾部（§19.4）：工具行 id → 缓冲。只在内存，随投影建、随投影丢。
    previews: BTreeMap<String, preview::Tail>,
    /// 输出上限之后的续写（§19.9）：续写提示之后记下的目标行；流式时已经接进目标行的消息。
    pending: Option<continuation::Continuation>,
    continued: BTreeMap<MessageId, continuation::Continuation>,
    /// 待回答的交互：实体 id → 内核的 id 与请求。
    interactions: BTreeMap<String, (InteractionId, InteractionRequest)>,
    /// 前端持有的待回答的校验（§35.2）：不在内核的视图里，重建时由 actor 带过来。
    verifications: verifications::Verifications,
    /// 内核的队列（最新一份）与每项第一次出现时的序号（`order.admissionSeq`）。
    queue: Vec<QueuedInput>,
    admitted: BTreeMap<CommandId, u64>,
    next_admission: u64,
    /// 输入的结局（最新）与终态的先后（保留上限 `INPUT_FATE_RETAIN`）。
    fates: BTreeMap<String, InputFate>,
    terminal: VecDeque<String>,
    /// 输入记录（内核的 `InputRecorded` 与视图的 `inputs`）：用户消息 id → 这条输入的显示事实（附件）。
    records: BTreeMap<MessageId, InputRecord>,
    /// 输入的消息 → 提交它的命令（内核的 `InputCommitted` 与视图的 `sources`，§24.1）：用户输入行的 `sourceCommandId`。
    sources: BTreeMap<MessageId, CommandId>,
    /// 回合 → 执行覆盖的退回（内核的 `ExecutionFellBack` 与视图的 `turns`，§46.1）：输入行的高速卡拼上它。
    fell_back: BTreeMap<String, rust_cli_kernel_api::turn_execution::FellBack>,
    /// 在跑的后台工作的 id（`works` 集合，§20.1）。
    works: BTreeSet<String>,
    /// 当前回合的工作段（§26.2）与工具结果的输出头部（视图的 `output_heads`，调用 id → v4 的 display，§26.3）。
    segments: segments::Segments,
    heads: BTreeMap<String, Value>,
    /// 视图里在跑回合因停止而取消的调用（§34）：结果在由视图建之后才到的，到时读成 cancelled（这一行可能还不在视图里）。
    cancelled: BTreeSet<String>,
    /// 这一回合里内核发过的只供显示的提示（`state.notices`，§28.1）：回合开始时清空；不落库，由视图建的投影没有。
    notices: Vec<Value>,
    /// ExitPlanMode 拿给用户批准的计划（§42.1）：调用 id → 计划，来自视图的 `plans` 与 `ToolDone`。
    plans: BTreeMap<String, rust_cli_kernel_api::plan::ShownPlan>,
    /// 这一轮的 UserPromptSubmit 拦下（§49.2）：回合结束时读它（成功结束时横幅留着）。
    hook_block: Option<hook_rows::HookBlock>,
    /// 子代理行的来源（§54.3）：物化时读的库里的那份与由视图建时还没放的行。
    subagents: subagent_rows::Subagents,
    /// 行 → 存储消息的索引（§71.2）：`rows.range` 碰到骨架行时只读这一页的消息。
    walk: walk::Walk,
    /// 投影只覆盖历史的一段后缀（§73.3）：下沿 = 建出的第一条存储消息；`earlier` = 它之前还有没读的历史。
    floor: Option<MessageId>,
    earlier: bool,
}

impl Projection {
    /// 由视图建投影与它的内容（冷读与活会话同一条路，§15.2）：全部行都是整行。
    pub(crate) fn build(view: &SessionView, origin: Origin) -> (Self, Content) {
        Self::built(view, origin, None)
    }

    /// 冷读只建窗口（§71.3）：建的过程里，落后最新的行 [`SESSION_SNAPSHOT_ROWS`] 行以上、不留整行的行直接换成骨架；之后照旧 `adopt` 与 `thin`，
    /// 结果与 [`Self::build`] 再 `thin` 逐项相同。只给冷读的物化用（冷视图没有在跑的回合，行建好之后不会再变成要留整行的）。
    ///
    /// 缺陷修复（PERF-WINDOW，用户定「冷读只建窗口」）：以前长会话的每一行都先建成整行进 Publisher，再整份换骨架（`thin`），打开 3000 回合的
    /// 会话时整行全在内存里、`thin` 再扫一遍。
    pub(crate) fn build_window(view: &SessionView, origin: Origin) -> (Self, Content) {
        Self::built(view, origin, Some(SESSION_SNAPSHOT_ROWS))
    }

    fn built(view: &SessionView, origin: Origin, window: Option<usize>) -> (Self, Content) {
        // 按存储的先后走：冷读里压缩点的保留段先挪回边界之前（R-COMPACT-TAIL-COLD，§62.1）
        let shown = compaction::stored_order(super::side_chat::shown(view));
        // 从哪里建（§73.1）：激活时从原来的下沿起；活视图里找不到它（别处改写了历史）就整段建，内容不同、发新快照
        let (skip, next_row, earlier) = match &origin.start {
            Start::Whole => (0, 0, false),
            Start::Tail => (0, ROW_NUMBER_ANCHOR, true),
            Start::Floor { record, number } => {
                match shown.iter().position(|message| message.record() == record) {
                    Some(at) => (at, *number, at > 0),
                    None => (0, 0, false),
                }
            }
        };
        let mut projection = Self {
            applied: view.last_seq,
            live: origin.live,
            draft: origin.draft && view.messages.is_empty() && view.running.is_none(),
            deleted: origin.deleted,
            running: None,
            outcome: None,
            error: None,
            retry: None,
            hold: view.queue_hold,
            title: view.meta.title.clone(),
            title_source: origin.title_source,
            side_chat: view.meta.kind == rust_cli_kernel_api::store::SessionKind::SideChat,
            mcp: origin.mcp,
            created_at: view.meta.created_at_ms,
            selection: view.selection.clone(),
            mode: view.mode,
            plan: view.plan,
            grant: view.mode_grant.clone(),
            plan_switch: view.plan_switch.clone(),
            follow_up: view.follow_up,
            context: view.context_usage,
            order: BTreeSet::new(),
            next_row,
            streams: BTreeMap::new(),
            open_tools: BTreeMap::new(),
            previews: BTreeMap::new(),
            pending: None,
            continued: BTreeMap::new(),
            interactions: BTreeMap::new(),
            verifications: BTreeMap::new(),
            queue: Vec::new(),
            admitted: BTreeMap::new(),
            next_admission: 0,
            fates: BTreeMap::new(),
            terminal: VecDeque::new(),
            records: view
                .inputs
                .iter()
                .map(|record| (record.message.clone(), record.clone()))
                .collect(),
            sources: view
                .sources
                .iter()
                .map(|source| (source.message.clone(), source.command.clone()))
                .collect(),
            fell_back: user_inputs::fell_back(view),
            works: BTreeSet::new(),
            segments: segments::Segments::default(),
            heads: view
                .output_heads
                .iter()
                .map(|kept| {
                    (
                        kept.tool_use.as_str().to_owned(),
                        rows::bash_display(kept.head.clone()),
                    )
                })
                .collect(),
            cancelled: view
                .cancelled
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            notices: Vec::new(),
            plans: files::plans(view),
            hook_block: None,
            subagents: subagent_rows::Subagents::new(&view.subagents, origin.subagents),
            walk: walk::Walk::starting_at(next_row),
            floor: None,
            earlier,
        };
        let mut edit = Edit::building();
        // 在跑回合的开始（session-lifecycle.md §10.35：视图的 `running`；会话的视图不会是子代理的运行）
        let started = view.running.as_ref().and_then(|running| running.turn());
        // 回合：有记录的按记录；没有记录的历史（Node 写的）按开回合的输入切出回合（§75.3），范围读补段首用同一个判定
        let mut turns = TurnWalk::new(&view.turns);
        // 主循环里的压缩（§50.2）：排在它开始时历史的最后一条消息之后，与活会话里出现的位置相同
        let loops = compaction::loop_markers(&view.turns);
        let mut turn = String::new();
        // 压缩回合的命令：这个回合里的压缩标记带它（§24.1）
        let mut compaction = None;
        // 只建窗口时已经看过的行数（§71.3）：它们之后的行按行号单调地往后加
        let mut behind = (0, 0);
        for message in shown[skip..].iter().copied() {
            projection
                .floor
                .get_or_insert_with(|| message.record().clone());
            match turns.step(message) {
                Some(Opened::Recorded(record)) => {
                    turn = record.turn.as_str().to_owned();
                    compaction = record
                        .command
                        .clone()
                        .filter(|_| record.origin == TurnOrigin::Compaction);
                    let running = started.map(|start| &start.turn) == Some(&record.turn);
                    // 已结束的回合按回合记录里的真实结局（§15.3.3）；较早写的、没有结局的记录读成完成
                    let state = match (&record.ended, running) {
                        (_, true) => turn_state::RUNNING,
                        (Some(ended), false) => rows::ended_state(ended.outcome),
                        (None, false) => turn_state::COMPLETED,
                    };
                    projection.header(&mut edit, &record.start(), state);
                    projection.turn_hooks(&mut edit, &view.hooks, &turn);
                    if !running {
                        projection.recorded_compaction(&mut edit, record);
                        projection.recorded_end(&mut edit, record);
                    }
                }
                Some(Opened::Unrecorded(id)) => {
                    turn = id.as_str().to_owned();
                    compaction = None;
                    projection.unrecorded_header(&mut edit, id, message.id());
                }
                None => {}
            }
            projection.message(&mut edit, &turn, compaction.as_ref(), message, true);
            for (of, index, run) in loops.get(message.id()).into_iter().flatten() {
                let running = started.map(|start| &start.turn) == Some(*of);
                projection.loop_compaction(&mut edit, (of.as_str(), *index), run, running);
            }
            if let Some(window) = window {
                behind = projection.outline_behind(&mut edit, window, behind);
            }
        }
        if let Some(start) = started {
            // 修 bug（§25.1）：以前视图只给在跑回合的 id，回合中途建的投影把压缩回合读成普通回合、没有开始时刻，补建的回合头没有来源
            // 与命令。视图现在带回合的开始（与 `TurnStarted` 同值），回合头与 `Running` 都由它建。
            if !projection
                .order
                .iter()
                .any(|(_, id)| id == start.turn.as_str())
            {
                projection.header(&mut edit, start, turn_state::RUNNING);
                projection.turn_hooks(&mut edit, &view.hooks, start.turn.as_str());
            }
            // 视图建的投影在物化之前没有 topic 的 seq（同 `works`，§20.1）；主循环里在跑的那次压缩取自回合记录（§50.2）
            let mut running = Running::of(start, 0);
            running.loop_compaction = view
                .turns
                .iter()
                .find(|record| record.turn == start.turn)
                .and_then(|record| {
                    record
                        .compactions
                        .iter()
                        .rposition(|run| run.ended.is_none())
                });
            projection.running = Some(running);
        }
        // 工具调用的起止（§31.1）：与活会话的 `ToolTimeRecorded` 同一个函数
        for time in &view.tool_times {
            projection.timed(&mut edit, time);
        }
        projection.close_recorded_unsettled(&mut edit, &view.turns);
        // 回合记录里因停止而取消的工具（02 §11 C7）：冷读与活会话同样是 cancelled。修 bug（§34）：在跑的回合还没有回合记录的结局，以前回合中途
        // 建的投影把停止时取消的行读成「出错」；视图另带在跑回合里取消的调用（与 `ToolDone{cancelled}` 同一次处理记下）
        for tool in view
            .turns
            .iter()
            .filter_map(|record| record.ended.as_ref())
            .flat_map(|ended| &ended.cancelled)
            .chain(&view.cancelled)
        {
            projection.cancel_row(&mut edit, tool.as_str());
        }
        for (interaction, request) in &view.pending {
            projection.asked(&mut edit, interaction, request);
        }
        projection.queued(&mut edit, &view.queue);
        projection.viewed_inputs(&mut edit, view);
        for work in &view.works {
            projection.work_started(&mut edit, work, 0);
        }
        // 撤销过文件改动的回合（§53.5）：回合头都建好之后标
        projection.files_restored(&mut edit, &view.reverted);
        projection.all_state(&mut edit);
        // 任务清单（K-S9，§50）：视图里的（冷读的视图也带）；之后由 `TasksChanged` 整体替换
        edit.state(state::keys::PLAN, state::plan(&view.tasks));
        (projection, edit.into_content())
    }

    /// 改写历史之后重建用的来源（§15.2）：仍是活的，草稿早已不在，标题来源与 `mcpAdmission` 沿用；新的历史整段建（§73.0 第 1 条）。
    pub(crate) fn origin(&self) -> Origin {
        Origin {
            live: true,
            draft: false,
            deleted: self.deleted,
            title_source: self.title_source,
            mcp: self.mcp.clone(),
            subagents: self.subagents.stored(),
            stamp: None,
            start: Start::Whole,
        }
    }

    /// 激活（冷 → 热）时由活视图建的那份从哪里起（§73.0 第 1 条）：沿用这一份的下沿与最早一行的号；下沿之前没有历史时也照样从下沿起，号才不变。
    pub(crate) fn activation(&self) -> Start {
        match (&self.floor, self.order.first()) {
            (Some(record), Some((number, _))) => Start::Floor {
                record: record.clone(),
                number: *number,
            },
            _ => Start::Whole,
        }
    }

    /// 下沿与「之前还有」（§73.3）：`rows.range` 往前补、目录与计划的读用它们。
    pub(crate) fn floor(&self) -> Option<&MessageId> {
        self.floor.as_ref()
    }

    pub(crate) fn earlier(&self) -> bool {
        self.earlier
    }

    /// 已知最早一行的号（往前补的段排在它之前）。
    pub(crate) fn first_number(&self) -> Option<u64> {
        self.order.first().map(|(number, _)| *number)
    }

    /// 下一个行号（往前补的段从 0 起建，按它平移，§73.3）。
    pub(crate) fn next_row(&self) -> u64 {
        self.next_row
    }

    /// 交出索引（往前补的段建完之后只要它，投影随即丢掉）。
    pub(crate) fn take_walk(&mut self) -> walk::Walk {
        std::mem::take(&mut self.walk)
    }

    /// 往前补的一段接到前面（§73.3）：行进全序、索引往前接、下沿与「之前还有」换成这一段的、这一段的输入记录并进来（重试与编辑读它）。
    /// 返回发 `rowsWindow` 的一批（行本身不发增量：client 从 `rows.range` 的答案拿）。
    pub(crate) fn prepend(&mut self, chunk: super::extend::Chunk) -> Batch {
        for (id, fields) in &chunk.rows {
            if let Some(number) = fields.get(CREATED_AT_SEQ).and_then(Value::as_u64) {
                self.order.insert((number, id.clone()));
            }
        }
        self.walk.prepend(chunk.walk, chunk.shift);
        if chunk.floor.is_some() {
            self.floor = chunk.floor;
        }
        self.earlier = chunk.earlier;
        for record in chunk.records {
            self.records.entry(record.message.clone()).or_insert(record);
        }
        let mut edit = Edit::new(None);
        self.window(&mut edit);
        edit.into_batch()
    }

    /// 按片重建（§71.2）用的视图的底子：会话的种类（侧聊的门与哪些消息成行看它）、模型与模式，不带消息与别的事实。
    pub(crate) fn bare_view(
        &self,
        mut meta: rust_cli_kernel_api::store::SessionMeta,
    ) -> SessionView {
        if self.side_chat {
            meta.kind = rust_cli_kernel_api::store::SessionKind::SideChat;
        }
        SessionView::new(meta, Vec::new(), self.selection.clone(), self.mode)
    }

    /// 物化时读的库里的子代理（§54.3）：冷读建、没被命令改过的投影上 `session.subagents` 直接用它（§70.0 第 1 条）。
    pub(crate) fn stored_subagents(&self) -> Vec<rust_cli_kernel_api::agents::StoredSubagent> {
        self.subagents.stored()
    }

    /// 本进程写下 MCP 信任之后（18 §12.6）：活会话换上写入之后的 `mcpAdmission`；冷会话没有准入过，不动（仍是 null）。
    pub(crate) fn mcp_admission(&mut self, value: &Value) -> Batch {
        let mut edit = Edit::new(None);
        if self.live {
            self.mcp = value.clone();
            edit.state(state::keys::MCP_ADMISSION, value.clone());
        }
        edit.into_batch()
    }

    pub(crate) fn interaction(&self, id: &str) -> Option<&(InteractionId, InteractionRequest)> {
        self.interactions.get(id)
    }

    /// 一条输入的消息的输入记录（落库之后不变，§15.3.3）：编辑缺省沿用、重试重发的原输入的附件与引用（protocol-v5.md §53.2）。
    pub(crate) fn record(&self, message: &MessageId) -> Option<&InputRecord> {
        self.records.get(message)
    }

    pub(crate) fn fate(&self, input: &str) -> Option<&InputFate> {
        self.fates.get(input)
    }

    /// 这条输入在内核队列里的投递方式（插话 / 排队）。
    pub(crate) fn queued_delivery(
        &self,
        input: &str,
    ) -> Option<rust_cli_kernel_api::session_api::Delivery> {
        self.queue
            .iter()
            .find(|queued| queued.command.as_str() == input)
            .map(|queued| queued.delivery)
    }

    pub(crate) fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// 投影里这一项排队的项（整项）：与 topic 里这一项的 `v` 同一步更新（protocol-v5.md §17.1、§23）。
    pub(crate) fn queued_item(&self, id: &str) -> Option<&QueuedInput> {
        self.queue
            .iter()
            .find(|queued| queued.command.as_str() == id)
    }

    pub(crate) fn deleted(&self) -> bool {
        self.deleted
    }

    /// 通道开着这个会话的运行时（由打开之后的视图建、没有被删）：投影的释放看它（protocol-v5.md §66.2）。
    pub(crate) fn runtime(&self) -> bool {
        self.live && !self.deleted
    }

    /// 本进程以 deferred 建、还没有回合的草稿（`phase: draft`）：没有可压缩的消息（§17.3）。
    pub(crate) fn draft(&self) -> bool {
        self.phase() == Phase::Draft
    }

    /// 一个内核事件 → 一批（`seq ≤ applied` 的丢掉，§15.2）。
    pub(crate) fn apply(&mut self, publisher: &Publisher, event: &SequencedEvent) -> Batch {
        if event.seq <= self.applied {
            return Batch::default();
        }
        self.applied = event.seq;
        let mut edit = Edit::new(Some(publisher));
        let before = (self.order.len(), self.order.first().cloned());
        self.event(&mut edit, &event.event, publisher.cursor().seq + 1);
        if (self.order.len(), self.order.first().cloned()) != before {
            self.window(&mut edit);
        }
        edit.into_batch()
    }

    /// 改名之后（`session.rename` 的回执）：标题来源是 `custom`（K-S3：`TitleChanged` 不带来源）。
    pub(crate) fn renamed(&mut self, title: &str) -> Batch {
        self.title = title.to_owned();
        self.title_source = TitleSource::Custom;
        let mut edit = Edit::new(None);
        edit.state(state::keys::META, self.meta());
        edit.into_batch()
    }

    fn event(&mut self, edit: &mut Edit<'_>, event: &KernelEvent, seq: u64) {
        match event {
            KernelEvent::SessionCreated { meta } => {
                self.title.clone_from(&meta.title);
                self.created_at = meta.created_at_ms;
                edit.state(state::keys::META, self.meta());
            }
            KernelEvent::TitleChanged { title } => {
                self.title.clone_from(title);
                if self.title_source == TitleSource::Default {
                    self.title_source = TitleSource::FirstInput;
                }
                edit.state(state::keys::META, self.meta());
            }
            KernelEvent::ModelChanged { selection } => {
                self.selection = selection.clone();
                edit.state(state::keys::CONFIG, self.config());
            }
            KernelEvent::ModeChanged { mode } => {
                self.mode = *mode;
                edit.state(state::keys::CONFIG, self.config());
            }
            KernelEvent::PlanChanged { enabled, by } => {
                self.plan = *enabled;
                // 工具切的 plan 是输入框要跟的事实（§18.2）；用户切的不动它
                if let Some(by) = by {
                    self.plan_switch = Some(PlanSwitch {
                        tool_use: by.clone(),
                        enabled: *enabled,
                    });
                }
                edit.state(state::keys::CONFIG, self.config());
            }
            KernelEvent::ModeGranted { tool_use } => {
                self.grant = Some(tool_use.clone());
                edit.state(state::keys::CONFIG, self.config());
            }
            // 跟随方式（§22.1）：回合在跑时 `admission.input.mode` 在 enqueue 与 guide 之间换，同一批里重算
            KernelEvent::FollowUpChanged { delivery } => {
                self.follow_up = *delivery;
                edit.state(state::keys::CONFIG, self.config());
                edit.state(state::keys::ADMISSION, self.admission());
            }
            KernelEvent::TurnStarted {
                turn,
                command,
                origin,
                started_at,
                works,
            } => {
                self.draft = false;
                self.retry = None;
                let start = TurnStart {
                    turn: turn.clone(),
                    command: command.clone(),
                    origin: *origin,
                    started_at: *started_at,
                    works: works.clone(),
                };
                self.running = Some(Running::of(&start, seq));
                // 提示属于产生它的那一回合（§28.1）：新的回合开始时清空（值没变时 Publisher 去掉这一项）
                self.notices.clear();
                edit.state(state::keys::NOTICES, json!([]));
                self.header(edit, &start, turn_state::RUNNING);
                self.control(edit);
                edit.state(state::keys::ADMISSION, self.admission());
            }
            KernelEvent::TurnFinished {
                turn,
                outcome,
                time,
                files,
                ..
            } => self.finished(edit, turn, outcome, *time, files),
            KernelEvent::StreamDelta {
                turn,
                message,
                index,
                tool_use,
                delta,
                ..
            } => {
                if self.retry.take().is_some() {
                    self.control(edit);
                }
                self.delta(
                    edit,
                    turn.as_str(),
                    message,
                    *index,
                    tool_use.as_ref(),
                    delta,
                );
            }
            KernelEvent::MessageDone { message } => {
                let turn = self
                    .running
                    .as_ref()
                    .map(|r| r.turn.as_str().to_owned())
                    .unwrap_or_default();
                let compaction = self.running.as_ref().and_then(|r| r.compaction().cloned());
                self.message(edit, &turn, compaction.as_ref(), message, false);
            }
            KernelEvent::ToolStarted {
                tool_use,
                name,
                input,
                started_at,
            } => self.tool_started(edit, tool_use, (name, input), *started_at),
            KernelEvent::ToolTimeRecorded { time } => self.timed(edit, time),
            // 修 bug（19-accept.md §15.3 D5）：以前进度落到下面的 `_ => {}`，前台 Bash 运行中看不到输出
            KernelEvent::ToolProgress { tool_use, progress } => {
                if let Some((output, total)) = preview::output_of(&progress.data) {
                    self.progressed(edit, tool_use.as_str(), output, total);
                }
            }
            KernelEvent::ToolDone {
                tool_use,
                output,
                cancelled,
            } => {
                let text = rust_cli_kernel_api::message::text_of(&output.content);
                // 修 bug（protocol-v5.md §21）：大输出的 Bash 给模型的是 `<persisted-output>`，界面要的头部在 `display` 里；以前只写给模型的
                // 文字，结果行显示了包装与临时文件路径。
                let display = CommandOutputHead::of(output).map(rows::bash_display);
                self.plan_shown(tool_use, output);
                self.result(edit, tool_use.as_str(), &text, output.is_error, display);
                // 因停止而结束（02 §11 C7）：结果是拒绝的文字，显示「已取消」，不是「出错」
                if *cancelled {
                    self.cancel_row(edit, tool_use.as_str());
                }
            }
            KernelEvent::InteractionAsked {
                interaction,
                request,
            } => self.asked(edit, interaction, request),
            KernelEvent::InteractionResolved { interaction } => self.resolved(edit, interaction),
            KernelEvent::ContextUsage {
                used_tokens,
                max_tokens,
                compact_at,
                window,
            } => {
                self.context = Some(ContextOccupancy {
                    used_tokens: *used_tokens,
                    max_tokens: *max_tokens,
                    compact_at: *compact_at,
                    window: *window,
                });
                edit.state(state::keys::USAGE, state::usage(self.context.as_ref()));
            }
            KernelEvent::QueueChanged { items } => {
                self.queued(edit, items);
                edit.state(state::keys::ADMISSION, self.admission());
            }
            KernelEvent::QueueHeld { hold } => {
                self.hold = *hold;
                edit.state(state::keys::QUEUE_POLICY, state::queue_policy(self.hold));
                edit.state(state::keys::ADMISSION, self.admission());
            }
            KernelEvent::InputFate { input, fate, at } => self.fated(edit, input, fate, *at),
            KernelEvent::HooksRan { hooks } => self.hooks_ran(edit, hooks),
            KernelEvent::SubagentRecorded { run } => self.subagent_recorded(edit, run),
            // 任务清单（§50）：事件带完整清单（agents 写的任务 JSON）。读不出时留着上一份（同 TUI：内核给的是整份，不会是半份）
            KernelEvent::TasksChanged { tasks } => {
                if let Ok(tasks) = serde_json::from_value::<Vec<TaskRecord>>(tasks.clone()) {
                    edit.state(state::keys::PLAN, state::plan(&tasks));
                }
            }
            KernelEvent::BackgroundStarted { work } => self.work_started(edit, work, seq),
            KernelEvent::BackgroundEnded { work } => self.work_ended(edit, work),
            // 记录先于它的消息到（§15.3.3）：建这条消息的行时取它；上报之后再来时重写这一行（§46.1）
            KernelEvent::InputRecorded { record } => self.recorded(edit, record),
            KernelEvent::ExecutionFellBack { turn, fell_back } => {
                self.fell_back(edit, turn, fell_back)
            }
            // 主循环里的压缩（§50.2）：回合记录里这一项变了
            KernelEvent::CompactionRecorded {
                turn,
                index,
                compaction,
            } => self.loop_recorded(edit, turn, *index, compaction),
            // 同上，关联先于它的消息到（§24.1）
            KernelEvent::InputCommitted { source } => {
                self.sources
                    .insert(source.message.clone(), source.command.clone());
            }
            KernelEvent::ModelNotice { notice, at } => self.model_notice(edit, notice, *at),
            // 修 bug（hooks.md §12 第 2 项）：以前它落进下面的 `_ => {}`，开关开时看不到 Stop hook 到上限的警告与被拦下的插话的说明。
            // 只供显示、不落库（§28.1）
            KernelEvent::SystemNotice { level, text, kind } => {
                self.notices
                    .push(state::notice(seq, *level, text, kind.as_ref()));
                edit.state(state::keys::NOTICES, Value::Array(self.notices.clone()));
            }
            // 撤销过文件改动的回合（§53.5）
            KernelEvent::FilesRestored { prompts } => self.files_restored(edit, prompts),
            // 改写历史由通道换成新的视图（§15.2）；`CompactDone` 之前边界消息的 `MessageDone` 已经建了标记行（§17.3）；主循环压缩的开始与失败
            // 看 `CompactionRecorded`，不看 `CompactStarted` / `CompactFailed`（§50.2）；别的事件不在投影里（只供显示、或没有对应的 v5 事实，§15.12）
            _ => {}
        }
    }

    /// 下一个行号（一行一个，§15.3.4），并记进全序。
    fn next(&mut self, id: &str) -> u64 {
        let seq = self.next_row;
        self.next_row += 1;
        self.order.insert((seq, id.to_owned()));
        seq
    }

    /// 已有的行的号，或新的号。
    fn seq_of(&mut self, edit: &Edit<'_>, id: &str) -> u64 {
        match edit.field(ROWS, id, CREATED_AT_SEQ).and_then(Value::as_u64) {
            Some(seq) => seq,
            None => self.next(id),
        }
    }

    fn remove_row(&mut self, edit: &mut Edit<'_>, id: &str) {
        if let Some(seq) = edit.field(ROWS, id, CREATED_AT_SEQ).and_then(Value::as_u64) {
            self.order.remove(&(seq, id.to_owned()));
            edit.remove(ROWS, id);
        }
    }

    /// 回合头：来源、开它的命令与开它的后台工作取自 `TurnStarted` 或回合记录（§24.1、§47.2）。
    fn header(&mut self, edit: &mut Edit<'_>, start: &TurnStart, state: &str) {
        let turn = start.turn.as_str();
        let seq = self.seq_of(edit, turn);
        self.segments = segments::Segments::new(Some(start.started_at));
        let mut fields = rows::turn_header(
            turn,
            seq,
            rows::origin(start.origin),
            start.started_at,
            state,
        );
        rows::set_command(&mut fields, start.command.as_ref());
        rows::set_origin_meta(&mut fields, &start.works);
        edit.put(ROWS, turn, fields);
        self.compaction_started(edit, start, state);
    }

    /// 一条消息的行（冷读与 `MessageDone` 同一个函数）。`cold` 时工具行按有没有结果定状态。`compaction` 是它所在的压缩回合的命令
    /// （不是压缩回合为 None）：压缩标记带它（§24.1）。
    fn message(
        &mut self,
        edit: &mut Edit<'_>,
        turn: &str,
        compaction: Option<&CommandId>,
        message: &Message,
        cold: bool,
    ) {
        self.walk.enter(message, turn);
        let created = message.id().created_ms();
        let mut kept = BTreeSet::new();
        let parts = rows::parts(message);
        // 输出上限之后的续写（§19.9）：接进上一条的正文行，不另起一行
        let joins = self.continuation_of(edit, turn, message, parts.first());
        self.segment_message(message);
        for part in parts {
            match part {
                Part::User { id, text, guided } => {
                    self.user_row(edit, (turn, message), (id, text, guided, created));
                }
                Part::Reasoning { id, text } => {
                    let seq = self.seq_of(edit, &id);
                    let fields = rows::answer(
                        kinds::REASONING,
                        &text,
                        DONE,
                        message.id().as_str(),
                        (turn, seq, created),
                    );
                    edit.put(ROWS, &id, fields);
                    kept.insert(id);
                }
                Part::Text { text, .. } if let Some(to) = &joins => {
                    self.join(edit, to, message.id().as_str(), &text);
                }
                Part::Text { id, text, model } => {
                    let seq = self.seq_of(edit, &id);
                    let mut fields = rows::answer(
                        kinds::ASSISTANT_TEXT,
                        &text,
                        DONE,
                        message.id().as_str(),
                        (turn, seq, created),
                    );
                    fields.insert(keys::MODEL.to_owned(), json!(model));
                    edit.put(ROWS, &id, fields);
                    kept.insert(id);
                }
                Part::Tool { id, name, input } => {
                    let seq = self.seq_of(edit, &id);
                    let mut fields = edit.get(ROWS, &id).cloned().unwrap_or_else(|| {
                        rows::tool_call(
                            tool_status::PENDING,
                            message.id().as_str(),
                            (turn, seq, created),
                        )
                    });
                    if rows::text(&fields, keys::STATUS) == tool_status::INPUT_STREAMING {
                        rows::set_status(&mut fields, tool_status::PENDING);
                    }
                    fields.insert(keys::TOOL_NAME.to_owned(), json!(name));
                    fields.insert(keys::INPUT_TEXT.to_owned(), json!(input.to_string()));
                    fields.insert(keys::INPUT.to_owned(), input);
                    self.with_plan(&id, &mut fields);
                    fields.insert(keys::RESPONSE.to_owned(), json!(message.id().as_str()));
                    if !tool_status::ended(rows::text(&fields, keys::STATUS)) {
                        self.open_tools.insert(id.clone(), turn.to_owned());
                    }
                    edit.put(ROWS, &id, fields);
                    kept.insert(id);
                }
                // 由视图建的投影按视图的输出头部补 display（§26.3）；活会话里 `ToolDone` 已经写了它，这里查不到也留着
                Part::Result { id, text, is_error } => {
                    let display = self.heads.get(&id).cloned();
                    self.result(edit, &id, &text, is_error, display);
                }
                Part::Marker {
                    id,
                    trigger,
                    tokens_before,
                } => {
                    let seq = self.boundary_seq(edit, turn, &id);
                    let mut fields = rows::marker(turn, seq, created, trigger, tokens_before);
                    rows::set_command(&mut fields, compaction);
                    edit.put(ROWS, &id, fields);
                }
            }
        }
        // 流式时建过、最终没有的行（思考块没结束、编号与最终不同）：去掉（§15.3.5）
        if !cold && let Some(stream) = self.streams.remove(message.id()) {
            for id in stream.rows.difference(&kept) {
                self.remove_row(edit, id);
            }
        }
        self.message_subagents(edit, message);
        self.walk.leave(self.next_row);
    }

    /// 行 → 存储消息的索引（§71.2）。
    pub(crate) fn walk(&self) -> &walk::Walk {
        &self.walk
    }

    /// 只建窗口（§71.3）：全序里排在最后 `window` 行之前、还没看过的行，不留整行的换成骨架。`seen` 是看过的行数与下一个要看的行号；冷读里
    /// 行号只往后加，所以没看过的就是行号不小于它的那些。返回新的 `seen`。
    fn outline_behind(
        &self,
        edit: &mut Edit<'_>,
        window: usize,
        seen: (usize, u64),
    ) -> (usize, u64) {
        let (count, from) = seen;
        let behind = self.order.len().saturating_sub(window);
        let Some(more) = behind.checked_sub(count).filter(|more| *more > 0) else {
            return seen;
        };
        let rows: Vec<&(u64, String)> = self
            .order
            .range((from, String::new())..)
            .take(more)
            .collect();
        let next = rows.last().map_or(from, |(seq, _)| seq + 1);
        let outside: Vec<&String> = rows
            .into_iter()
            .map(|(_, id)| id)
            .filter(|id| {
                edit.get(ROWS, id)
                    .is_some_and(|fields| !self.resident(fields))
            })
            .collect();
        for id in outside {
            edit.outline(ROWS, id, residence::OUTLINE);
        }
        (behind, next)
    }
}

#[path = "projection_compact.rs"]
mod compaction;
#[path = "projection_continue.rs"]
mod continuation;
#[path = "projection_edit.rs"]
mod edit;
#[path = "projection_files.rs"]
mod files;
#[path = "projection_hooks.rs"]
mod hook_rows;
#[cfg(test)]
#[path = "projection_test_kit.rs"]
pub(crate) mod kit;
#[path = "projection_lists.rs"]
mod lists;
#[path = "projection_segments.rs"]
mod segments;
#[path = "projection_stream.rs"]
mod stream;
#[path = "projection_subagents.rs"]
mod subagent_rows;
#[path = "projection_tools.rs"]
mod tools;
#[path = "projection_turns.rs"]
mod turns;
#[path = "projection_inputs.rs"]
mod user_inputs;
#[path = "projection_verify.rs"]
pub(crate) mod verifications;

#[path = "projection_residence.rs"]
pub(crate) mod residence;
#[path = "projection_rewind.rs"]
mod rewind;
#[path = "projection_walk.rs"]
pub(crate) mod walk;
#[path = "projection_works.rs"]
mod works;

#[cfg(test)]
#[path = "projection_hooks_tests.rs"]
mod hooks_tests;
#[cfg(test)]
#[path = "projection_notices_tests.rs"]
mod notices_tests;
#[cfg(test)]
#[path = "projection_origin_tests.rs"]
mod origin_tests;
#[cfg(test)]
#[path = "projection_plan_tests.rs"]
mod plan_tests;
#[cfg(test)]
#[path = "projection_scale_tests.rs"]
mod scale_tests;
#[cfg(test)]
#[path = "projection_tail_tests.rs"]
mod tail_tests;
#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "projection_turns_tests.rs"]
mod turns_tests;
#[cfg(test)]
#[path = "projection_window_tests.rs"]
mod window_tests;
