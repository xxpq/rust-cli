//! 会话的通道（protocol-v5.md §15.1）：每个会话一个任务。串行执行这个会话的命令（同一会话的命令按到达顺序进内核的
//! mailbox），并把内核的事件流原样转给连接的 actor。通道只是管道：投影、topic 与出站队列都在 actor 里。
//!
//! ```text
//! actor ─ LaneRequest（Read、Run、Query、Close、Check）─► 通道 ─ Kernel / SessionHandle ─► 内核的会话 actor
//! actor ◄─ Event::Lane{sid, generation, LaneEvent}（Viewed、Kernel、Rebuilt、Failed、Done、Closed、Checked）─ 通道   一个 FIFO，按产生的顺序
//! actor ◄─ Event::RuntimeRead ─ 读的任务（Query：借通道的 handle，不挡通道的循环，protocol-v5.md §43.3）
//! ```
//!
//! ACK 排在效果之后（G2）：内核的会话 actor 先把事件追加进日志、再回执，所以回执到达时效果事件已在流里；通道先把流里已有的
//! 事件取空（不等）并转发，再转发回执。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use futures_util::{FutureExt, StreamExt};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{CommandId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, EndReason, KernelError, KernelEvent,
    Persistence, SequencedEvent, SessionHandle, SessionView, StartSource,
};
use rust_cli_kernel_api::session_facts::{SessionFacts, TitleSource};
use rust_cli_kernel_api::session_preferences::OpenStart;
use rust_cli_kernel_api::store::StoreQueries;
use serde_json::Value;
use tokio::sync::mpsc;

use super::projection::{Origin, Start};
use super::queries::ok;
use super::reads::Read;
use super::retain::Stamp;
use crate::consts::TAIL_RECORDS;
use crate::generated::contract::codes;
use crate::generated::core_v1::Cursor;
use crate::server::{Event, WorkspaceKernel, Workspaces};

/// actor 交给通道的请求。
pub(crate) enum LaneRequest {
    /// 订阅要视图（§15.2）：打开着就取活视图，否则冷读（不起 actor、不跑 SessionStart）。
    Read,
    /// 一条命令（`ticket` 原样带回）。
    Run { ticket: Ticket, op: Box<Operation> },
    /// 一条要会话运行时的查询（§43.2）：`reply` 是 JSON-RPC 的 id，`cursor` 是 actor 收到它时 topic 的位置。
    Query {
        reply: Value,
        read: Read,
        cursor: Cursor,
    },
    /// 关掉会话运行时（热会话按上限回收，protocol-v5.md §69）：会话 actor 判空闲才关；答 `Closed`。
    Close,
    /// 保留的投影要重订（protocol-v5.md §70）：读一次库里的戳（会话事实与分支切点），与保留时的比；答 `Checked`，读出拒绝时答 `Failed`。
    Check(Box<Stamp>),
}

/// 一条在途的命令：JSON-RPC 的 id、v5 的 commandId（内核命令的 id 就是它，内核的账本按它去重）、命令类型（结果按它换算）与
/// `expect` 指向的实体（内核判出「变了」时，回执带它现在的值，protocol-v5.md §17.1）。
#[derive(Debug, Clone)]
pub(crate) struct Ticket {
    pub(crate) reply: Value,
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) entity: Option<String>,
}

/// 通道执行的一步（actor 已经校验过参数、按投影判过 expect）。
#[allow(
    clippy::large_enum_variant,
    reason = "一条命令一个、经通道一个一个地传，不成批存放；输入带上这一轮的执行覆盖之后提交的变体过了门槛，装箱只多一次分配"
)]
pub(crate) enum Operation {
    Create {
        persistence: Persistence,
        /// `executionDirectories`（protocol-v5.md §32）：建会话时的附加工作目录。
        directories: Vec<rust_cli_kernel_api::env::EnvPath>,
    },
    Rename(String),
    Delete,
    /// 要打开会话的一步。
    Live(LiveOp),
    /// 在这个会话里分出侧聊（protocol-v5.md §57.2）：`command` 是内核的 `SideChatAs`；`first` 是侧聊的首条输入，分出之后由连接的 actor
    /// 交给侧聊的通道（通道只是管道，不碰别的会话）。
    SideChat {
        command: Command,
        first: Option<Command>,
    },
}

/// 要打开会话（激活）才能做的一步。
pub(crate) enum LiveOp {
    /// 一条内核命令（停止、队列、交互、普通的提交）。
    Kernel(Command),
    SetModel(ModelSelection),
    /// 会话的跟随方式（§21）：与视图相同时 noop。
    SetFollowUp(Delivery),
    SetMode {
        mode: PermissionMode,
        plan: bool,
    },
    /// 编辑重发（protocol-v5.md §53.2）：内核的 `ReplaceInput`；文件恢复不了时另问一次预览，放进结果。
    Edit {
        command: Command,
        target: rust_cli_kernel_api::ids::MessageId,
    },
}

/// 命令的结局（actor 按命令换成 v5 的结果，§15.4.2）。
pub(crate) enum Outcome {
    Ack(Ack),
    /// 编辑的回执与它被挡住时的预览（§53.2）。
    Edited {
        ack: Ack,
        preview: Option<Value>,
    },
    Created(Result<(), KernelError>),
    /// 设值语义、值没变：什么都没发（02 §6 的 `noop`）。
    Noop,
    Renamed(String),
    Deleted,
    /// 通道这一侧的拒绝（找不到、删除了、别的工作区）或失败（`internal.failed`）。
    Refused(Rejection),
}

/// 通道这一侧的拒绝：已登记的码与诊断原文。码是 `internal.failed` 时它是失败（02 §6：failed 不是 rejected）。
#[derive(Debug, Clone)]
pub(crate) struct Rejection {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

/// 通道给 actor 的消息。
pub(crate) enum LaneEvent {
    Viewed {
        view: Box<SessionView>,
        origin: Origin,
    },
    Kernel(Box<SequencedEvent>),
    /// 历史被改写（`HistoryRewritten`）：新的视图，之后的事件从它的 `last_seq` 接着（§15.2）。
    Rebuilt(Box<SessionView>),
    /// 读视图失败（等着的订阅各收 `subscription.failed`）。
    Failed(Rejection),
    Done {
        ticket: Ticket,
        outcome: Outcome,
        /// 侧聊的首条输入（[`Operation::SideChat`]，§57.2）：actor 按回执决定接着交给侧聊的通道，还是直接回 ACK。装箱：命令比回执大得多。
        then: Option<Box<Command>>,
    },
    /// `Close` 的结局（§69）：`cold` 是之后通道没有会话运行时（关了）；会话 actor 判出不空闲时为 false，什么都没变。
    Closed {
        cold: bool,
    },
    /// `Check` 的结局（§70）：库里的会话事实与保留时的相同。
    Checked {
        same: bool,
    },
}

/// 起通道要的东西（进程级部件建好之后才有）。
pub(crate) struct LaneParts {
    pub(crate) sid: SessionId,
    pub(crate) key: String,
    pub(crate) generation: u64,
    pub(crate) workspaces: Arc<dyn Workspaces>,
    pub(crate) queries: Arc<dyn StoreQueries>,
    pub(crate) out: mpsc::UnboundedSender<Event>,
}

struct Lane {
    parts: LaneParts,
    /// 起读的任务（§43.3）。
    spawner: rust_cli_base::Spawner,
    /// 打开（或建）这个会话的那一套内核：通道打开过会话、还没关时有；冷读不记（protocol-v5.md §72.9）。
    kernel: Option<WorkspaceKernel>,
    handle: Option<Arc<dyn SessionHandle>>,
    events: Option<BoxStream<'static, SequencedEvent>>,
    /// 已转发到的内核 seq：事件流落后而结束时从这里重订（§15.2 第 3 条）。
    applied: u64,
    /// 本通道以 deferred 建的草稿（投影的 `phase: draft`）。
    draft: bool,
    /// 这个会话是不是侧聊（会话事实的种类，§57.4）：第一次遇到侧聊不接受的命令时读一次，种类不变。
    side_chat: Option<bool>,
}

fn rejection(code: &'static str, message: impl Into<String>) -> Rejection {
    Rejection {
        code,
        message: message.into(),
    }
}

/// 组装工作区、存储与内核的失败（不是可预期的拒绝）。
fn failed(message: impl Into<String>) -> Rejection {
    rejection(codes::INTERNAL_FAILED, message)
}

/// 存储里的事实：这个会话是不是别的工作区的、删除了没有（KERNEL-B 差异 5：删除之后内核里没有「已删除的 actor」）。库里没有这个会话时为 None。
async fn check_facts(
    queries: Arc<dyn StoreQueries>,
    workspace: WorkspaceRef,
    sid: SessionId,
) -> Result<Option<SessionFacts>, Rejection> {
    let facts = queries
        .session_facts(std::slice::from_ref(&sid))
        .await
        .map_err(|error| rejection(codes::PRECONDITION_STORE_UNAVAILABLE, error.to_string()))?;
    let Some(facts) = facts.into_iter().next() else {
        return Ok(None);
    };
    if facts.workspace != workspace {
        return Err(rejection(
            codes::PRECONDITION_WORKSPACE_MISMATCH,
            "the session belongs to another workspace",
        ));
    }
    if facts.deleted_at_ms.is_some() {
        return Err(rejection(
            codes::ADMISSION_SESSION_DELETED,
            "the session was deleted",
        ));
    }
    Ok(Some(facts))
}

/// 库里的戳（protocol-v5.md §70.0 第 2 条）：读到了会话事实时再读分支切点。库里没有这个会话时没有戳（不保留）。
async fn stamp(
    queries: &dyn StoreQueries,
    facts: Option<SessionFacts>,
) -> Result<Option<Stamp>, Rejection> {
    let Some(facts) = facts else {
        return Ok(None);
    };
    Stamp::read(queries, facts)
        .await
        .map(Some)
        .map_err(|error| match error {
            rust_cli_kernel_api::store::StoreError::NotFound { .. } => {
                rejection(codes::PRECONDITION_NOT_FOUND, error.to_string())
            }
            other => rejection(codes::PRECONDITION_STORE_UNAVAILABLE, other.to_string()),
        })
}

/// 会话的标题来源：库里没有这个会话（还没落库的草稿）时是缺省的。
fn title_source(facts: Option<&SessionFacts>) -> TitleSource {
    facts.map_or(TitleSource::Default, |facts| facts.title_source)
}

/// 库里的子代理（子代理行的冷读，§54.3）：读视图时一起读，在通道的任务里（不在链路的 actor 里）。
async fn subagents(
    queries: &dyn StoreQueries,
    sid: &SessionId,
) -> Result<Vec<rust_cli_kernel_api::agents::StoredSubagent>, Rejection> {
    super::reads::stored_subagents(queries, sid)
        .await
        .map_err(|error| rejection(codes::PRECONDITION_STORE_UNAVAILABLE, error.to_string()))
}

/// 冷读一个会话（不起会话 actor、不跑 SessionStart，02 §8.2）：先看存储里的事实，再组装工作区、读快照。订阅的物化（通道的 `Read`）
/// 与没有物化的会话的只读查询（protocol-v5.md §40.2）用这一个函数，两边的行是同一组。
pub(crate) async fn cold_view(
    workspaces: Arc<dyn Workspaces>,
    queries: Arc<dyn StoreQueries>,
    key: String,
    sid: SessionId,
) -> Result<(WorkspaceKernel, SessionView, Origin), Rejection> {
    // 戳（事实与分支切点）先于快照读（protocol-v5.md §70.4）：两次读之间别处写了，投影比记下的戳新，重订时只会多重建一次
    let facts = check_facts(queries.clone(), workspaces.workspace(&key), sid.clone()).await?;
    let title_source = title_source(facts.as_ref());
    let stamp = stamp(queries.as_ref(), facts).await?;
    let kernel = workspaces.open(&key).await.map_err(failed)?;
    // 只读最后一段（§73.2）：子代理只要这一段里的 Agent 调用，随这次读一起读出（以前另读整段的 Agent part）
    let tail = kernel
        .kernel
        .session_tail(&sid, TAIL_RECORDS)
        .await
        .map_err(not_found)?;
    // 侧聊从它的起点之前的历史不显示（§57.5）：段里有起点就不必再往前
    let earlier = tail.earlier && !super::side_chat::starts_in(&tail.view);
    let origin = Origin {
        live: false,
        draft: false,
        deleted: false,
        title_source,
        // 冷读不起会话 actor，没有准入过（18 §12.5）
        mcp: Value::Null,
        subagents: tail.subagents,
        stamp,
        start: if earlier { Start::Tail } else { Start::Whole },
    };
    Ok((kernel, tail.view, origin))
}

/// 起一条通道：返回它的请求入口。请求入口关了（actor 换了通道或进程退出）时任务结束。
pub(crate) fn start(
    parts: LaneParts,
    spawner: &rust_cli_base::Spawner,
) -> mpsc::UnboundedSender<LaneRequest> {
    let (requests, receiver) = mpsc::unbounded_channel();
    let lane = Lane {
        parts,
        spawner: spawner.clone(),
        kernel: None,
        handle: None,
        events: None,
        applied: 0,
        draft: false,
        side_chat: None,
    };
    drop(spawner.spawn("protocol-v5-session", lane.run(receiver)));
    requests
}

/// 下一个内核事件；没有事件流时永远等（select 里只看请求）。
async fn next_event(
    events: &mut Option<BoxStream<'static, SequencedEvent>>,
) -> Option<SequencedEvent> {
    match events {
        Some(events) => events.next().await,
        None => std::future::pending().await,
    }
}

impl Lane {
    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<LaneRequest>) {
        loop {
            tokio::select! {
                biased;
                request = requests.recv() => match request {
                    Some(request) => self.handle(request).await,
                    None => return,
                },
                event = next_event(&mut self.events) => match event {
                    Some(event) => self.forward(event).await,
                    // 落后（广播缓冲满）：从已转发的位置重订，日志里留着之后的事件（session/src/log.rs）
                    None => self.resubscribe(),
                },
            }
        }
    }

    fn emit(&self, event: LaneEvent) {
        let _ = self.parts.out.send(Event::Lane {
            sid: self.parts.sid.as_str().to_owned(),
            generation: self.parts.generation,
            event: Box::new(event),
        });
    }

    fn resubscribe(&mut self) {
        self.events = self
            .handle
            .as_ref()
            .map(|handle| handle.subscribe(Some(self.applied)));
    }

    async fn forward(&mut self, event: SequencedEvent) {
        self.applied = event.seq;
        if matches!(event.event, KernelEvent::HistoryRewritten { .. }) {
            // 改写历史：取新的视图、从它接着订（§15.2）。取不到视图时会话已经关了，事件流随之结束
            if let Some(handle) = self.handle.clone()
                && let Ok(view) = handle.view().await
            {
                self.applied = view.last_seq;
                self.events = Some(handle.subscribe(Some(view.last_seq)));
                self.emit(LaneEvent::Rebuilt(Box::new(view)));
            }
            return;
        }
        self.emit(LaneEvent::Kernel(Box::new(event)));
    }

    /// 流里已经有的事件都转发出去（不等）：回执之前的效果先到 actor（G2）。
    async fn drain(&mut self) {
        while let Some(events) = self.events.as_mut()
            && let Some(Some(event)) = events.next().now_or_never()
        {
            self.forward(event).await;
        }
    }

    async fn handle(&mut self, request: LaneRequest) {
        match request {
            LaneRequest::Read => self.read().await,
            LaneRequest::Run { ticket, mut op } => {
                let refused = self.refuse_on_side_chat(&ticket.kind, &op).await;
                // 首条输入不在父会话的通道里执行（通道只管自己的会话）：随回执交回 actor
                let then = match op.as_mut() {
                    Operation::SideChat { first, .. } => first.take().map(Box::new),
                    _ => None,
                };
                let outcome = match refused {
                    Some(rejection) => Outcome::Refused(rejection),
                    None => self.run_op(ticket.id.clone(), *op).await,
                };
                self.drain().await;
                self.emit(LaneEvent::Done {
                    ticket,
                    outcome,
                    then,
                });
            }
            LaneRequest::Query {
                reply,
                read,
                cursor,
            } => self.query(reply, read, cursor),
            LaneRequest::Close => self.close().await,
            LaneRequest::Check(known) => self.check(*known).await,
        }
    }

    /// 保留的投影还是不是库里的样子（protocol-v5.md §70.0 第 2 条）：同冷读的第一步读戳（会话事实与分支切点），与保留时的比。删除了、别的工作区、
    /// 库不可用照冷读失败答。
    async fn check(&mut self, known: Stamp) {
        let read = match self.check_facts().await {
            Ok(facts) => stamp(self.parts.queries.as_ref(), facts).await,
            Err(rejection) => Err(rejection),
        };
        match read {
            Ok(current) => self.emit(LaneEvent::Checked {
                same: current.as_ref() == Some(&known),
            }),
            Err(rejection) => self.emit(LaneEvent::Failed(rejection)),
        }
    }

    /// 热会话回收（§69.0 第 3、6 条）：先把流里已有的事件转给 actor，再请会话 actor 判空闲并关；关了就再取空一次、丢 handle（内核已把会话
    /// 移出会话表，历史与按会话的表随之释放）。
    async fn close(&mut self) {
        if let Some(handle) = self.handle.clone() {
            self.drain().await;
            if handle.close_idle(EndReason::Other).await {
                self.drain().await;
                self.handle = None;
                self.events = None;
                self.kernel = None;
            }
        }
        self.emit(LaneEvent::Closed {
            cold: self.handle.is_none(),
        });
    }

    /// 要会话运行时的查询（§43.2）：起读的任务（不等它，通道照常转发事件）；打开着会话时借它的 handle，没有打开（冷的 topic、删除之后、
    /// 回收之后）时没有。子代理目录在任务里另读库（§45.3）。结果直接回 actor，带会话 id（在途的读挡着回收，§69.3）。
    fn query(&self, reply: Value, read: Read, cursor: Cursor) {
        let out = self.parts.out.clone();
        let (handle, queries, sid) = (
            self.handle.clone(),
            self.parts.queries.clone(),
            self.parts.sid.clone(),
        );
        drop(self.spawner.spawn("protocol-v5-session-query", async move {
            let session = sid.as_str().to_owned();
            let result = read
                .answer(handle, queries, sid)
                .await
                .map(|data| ok(data, cursor));
            let _ = out.send(Event::RuntimeRead {
                sid: session,
                reply,
                result,
            });
        }));
    }

    /// 通道打开过这个会话（还没关）时是打开它的那一套（钉住：会话的插件、工具表不随工作区变）；否则取工作区现在的那一套。
    /// 打开之后读视图失败、`handle` 没记下时也用钉住的那一套：会话已在那一套里开着，换一套再打开就是两个会话 actor。
    ///
    /// 修改原因（PLUG-P4B，protocol-v5.md §72.9）：以前通道第一次拿到内核（冷读、建会话、激活）就一直留着。插件改了、工作区内核被
    /// 拿走之后，冷读过的会话再激活、热回收之后再打开，仍在旧内核上，用的是改之前的插件（Node：冷恢复用最新配置）。
    async fn kernel(&mut self) -> Result<WorkspaceKernel, String> {
        if let Some(kernel) = &self.kernel {
            return Ok(kernel.clone());
        }
        self.parts.workspaces.open(&self.parts.key).await
    }

    /// 侧聊会话不接受的一步（§57.4）：种类是会话行的事实（唯一来源是 `task_type`），冷的、没订阅的侧聊同样拒绝。只在禁止的命令上读一次事实。
    /// 读不到（会话不在）时不拒绝：这一步照常走，由它自己判出找不到。
    async fn refuse_on_side_chat(&mut self, kind: &str, op: &Operation) -> Option<Rejection> {
        if !super::side_chat::forbidden(kind, op) {
            return None;
        }
        if self.side_chat.is_none() {
            let facts = self
                .parts
                .queries
                .session_facts(std::slice::from_ref(&self.parts.sid))
                .await;
            match facts {
                // 会话还不在时不记：之后建出来的才知道种类
                Ok(facts) => {
                    self.side_chat = facts.first().map(|facts| {
                        facts.kind == rust_cli_kernel_api::store::SessionKind::SideChat
                    });
                }
                Err(error) => {
                    let code = codes::PRECONDITION_STORE_UNAVAILABLE;
                    return Some(rejection(code, error.to_string()));
                }
            }
        }
        (self.side_chat == Some(true)).then(|| super::side_chat::refused(kind))
    }

    /// 这个会话在存储里的事实（[`check_facts`]）。返回的 future 不借 `self`（通道的事件流不是 `Sync`）。
    fn check_facts(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<SessionFacts>, Rejection>> + Send + 'static
    {
        check_facts(
            self.parts.queries.clone(),
            self.parts.workspaces.workspace(&self.parts.key),
            self.parts.sid.clone(),
        )
    }

    async fn read(&mut self) {
        // 已经打开着：再给一份活视图（actor 那边还没有投影时才会来读）
        if let Some(handle) = self.handle.clone() {
            if let Err(rejection) = self.view_live(&handle).await {
                self.emit(LaneEvent::Failed(rejection));
            }
            return;
        }
        let read = cold_view(
            self.parts.workspaces.clone(),
            self.parts.queries.clone(),
            self.parts.key.clone(),
            self.parts.sid.clone(),
        );
        match read.await {
            // 冷读不钉内核（§72.9）：之后激活时取那时的那一套
            Ok((_, view, origin)) => {
                self.emit(LaneEvent::Viewed {
                    view: Box::new(view),
                    origin,
                });
            }
            Err(rejection) => self.emit(LaneEvent::Failed(rejection)),
        }
    }

    /// 打开会话（激活，§15.2）；已经打开着时直接给出。
    async fn activate(&mut self) -> Result<Arc<dyn SessionHandle>, Rejection> {
        self.open(true).await
    }

    /// 附加目录与会话偏好不落库：冷打开之前问 Host 一次（`ask_host`；两个请求同时发、各等各的回复），作为会话的起始状态交给内核
    /// （§37、§48）。这期间这个会话的命令排在通道里，第一个回合在它们之后；会话 actor 起来时它们已在起始状态里。接上刚建的会话时不问：
    /// 建会话带的就是 Host 预留时算的目录与建会话之前问到的偏好。
    async fn open(&mut self, ask_host: bool) -> Result<Arc<dyn SessionHandle>, Rejection> {
        if let Some(handle) = &self.handle {
            return Ok(handle.clone());
        }
        let kernel = self.kernel().await.map_err(failed)?;
        self.open_in(kernel, ask_host).await
    }

    /// 在这一套内核里打开会话，打开了就钉住它（§72.9）；已经开着时直接给出。
    async fn open_in(
        &mut self,
        kernel: WorkspaceKernel,
        ask_host: bool,
    ) -> Result<Arc<dyn SessionHandle>, Rejection> {
        if let Some(handle) = &self.handle {
            return Ok(handle.clone());
        }
        self.check_facts().await?;
        let start = if ask_host {
            let (out, sid) = (&self.parts.out, self.parts.sid.as_str());
            let (directories, preferences) = tokio::join!(
                crate::host_services::session_directories(out, sid),
                crate::host_services::session_preferences(out, sid),
            );
            OpenStart {
                directories,
                preferences,
            }
        } else {
            OpenStart::default()
        };
        let handle = kernel
            .kernel
            .open_session_with(&self.parts.sid, StartSource::Resume, start)
            .await
            .map_err(not_found)?;
        // 会话已在这一套里开着：先钉住，读视图失败时之后的命令也只进这一套
        self.kernel = Some(kernel);
        self.view_live(&handle).await?;
        Ok(handle)
    }

    /// 先读视图、再从它的 `last_seq` 订事件（§15.2 的物化顺序），把视图交给 actor。会话 actor 已经启动（它的 MCP 准入已经开始），
    /// 这时读一次信任记录算 `mcpAdmission`（18 §12.6）。
    async fn view_live(&mut self, handle: &Arc<dyn SessionHandle>) -> Result<(), Rejection> {
        let title_source = title_source(self.check_facts().await?.as_ref());
        let view = handle.view().await.map_err(not_found)?;
        self.applied = view.last_seq;
        self.events = Some(handle.subscribe(Some(view.last_seq)));
        self.handle = Some(handle.clone());
        let pending = self.parts.workspaces.mcp_pending(&self.parts.key).await;
        let subagents = subagents(self.parts.queries.as_ref(), &self.parts.sid).await?;
        self.emit(LaneEvent::Viewed {
            view: Box::new(view),
            origin: Origin {
                live: true,
                draft: self.draft,
                deleted: false,
                title_source,
                mcp: crate::mcp_trust::admission(&pending),
                subagents,
                // 活视图：投影之后跟着内核的事件走，不是库里某一份的样子（§70）
                stamp: None,
                // 整段建；已有冷的投影时由 actor 换成沿用它的下沿（§73.0 第 1 条）
                start: Start::Whole,
            },
        });
        Ok(())
    }

    /// 发一条内核命令；等回执时照常转发事件（长命令期间流式不停）。
    async fn send(&mut self, handle: &Arc<dyn SessionHandle>, id: String, command: Command) -> Ack {
        let envelope = CommandEnvelope {
            id: CommandId::new(id),
            command,
        };
        let sending = handle.send(envelope);
        tokio::pin!(sending);
        loop {
            tokio::select! {
                biased;
                ack = &mut sending => return ack,
                event = next_event(&mut self.events) => match event {
                    Some(event) => self.forward(event).await,
                    None => self.resubscribe(),
                },
            }
        }
    }

    async fn run_op(&mut self, id: String, op: Operation) -> Outcome {
        match op {
            Operation::Create {
                persistence,
                directories,
            } => self.create(persistence, directories).await,
            Operation::Rename(title) => match self.kernel().await {
                Ok(kernel) => match kernel
                    .kernel
                    .rename_session(&self.parts.sid, title.clone())
                    .await
                {
                    Ok(()) => Outcome::Renamed(title),
                    Err(error) => Outcome::Refused(not_found(error)),
                },
                Err(message) => Outcome::Refused(failed(message)),
            },
            Operation::Delete => match self.kernel().await {
                Ok(kernel) => match kernel.kernel.delete_session(&self.parts.sid).await {
                    Ok(()) => {
                        // 内核关了会话的 actor（session-facts.md §5）：之后的命令看事实里的删除时刻
                        self.handle = None;
                        self.events = None;
                        self.kernel = None;
                        Outcome::Deleted
                    }
                    Err(error) => Outcome::Refused(not_found(error)),
                },
                Err(message) => Outcome::Refused(failed(message)),
            },
            Operation::Live(op) => match self.activate().await {
                Ok(handle) => self.live_op(&handle, id, op).await,
                Err(rejection) => Outcome::Refused(rejection),
            },
            Operation::SideChat { command, .. } => match self.activate().await {
                Ok(handle) => Outcome::Ack(self.send(&handle, id, command).await),
                Err(rejection) => Outcome::Refused(rejection),
            },
        }
    }

    async fn live_op(
        &mut self,
        handle: &Arc<dyn SessionHandle>,
        id: String,
        op: LiveOp,
    ) -> Outcome {
        match op {
            LiveOp::Kernel(command) => Outcome::Ack(self.send(handle, id, command).await),
            LiveOp::Edit { command, target } => {
                let ack = self.send(handle, id, command).await;
                // 内核在回执之前什么都没改；预览与这之后文件的变化不保证先后，只供冲突框显示（§53.2）。会话在回执之后关了：没有预览
                let preview = if super::rewind::blocked(&ack) {
                    handle
                        .rewind_preview(&target)
                        .await
                        .ok()
                        .flatten()
                        .map(|preview| super::rewind::preview_result(&preview))
                } else {
                    None
                };
                Outcome::Edited { ack, preview }
            }
            LiveOp::SetModel(selection) => match handle.view().await {
                Ok(view) if view.selection.same_choice(&selection) => Outcome::Noop,
                Ok(_) => Outcome::Ack(self.send(handle, id, Command::SetModel { selection }).await),
                Err(error) => Outcome::Refused(not_found(error)),
            },
            LiveOp::SetFollowUp(delivery) => match handle.view().await {
                Ok(view) if view.follow_up == delivery => Outcome::Noop,
                Ok(_) => Outcome::Ack(
                    self.send(handle, id, Command::SetFollowUp { delivery })
                        .await,
                ),
                Err(error) => Outcome::Refused(not_found(error)),
            },
            LiveOp::SetMode { mode, plan } => {
                let view = match handle.view().await {
                    Ok(view) => view,
                    Err(error) => return Outcome::Refused(not_found(error)),
                };
                if view.mode == mode && view.plan == plan {
                    return Outcome::Noop;
                }
                // 两条内核命令：模式与 plan 开关分开（修复线 F20）；先改模式，被拒就停
                if view.mode != mode {
                    let ack = self
                        .send(handle, format!("{id}:mode"), Command::SetMode { mode })
                        .await;
                    if matches!(ack, Ack::Rejected { .. }) || view.plan == plan {
                        return Outcome::Ack(ack);
                    }
                }
                Outcome::Ack(
                    self.send(
                        handle,
                        format!("{id}:plan"),
                        Command::SetPlan { enabled: plan },
                    )
                    .await,
                )
            }
        }
    }

    /// 建会话（KERNEL-B §6：调用方给的 id 幂等），再打开它（通道激活）。`directories` 是会话的附加工作目录（session-dirs.md
    /// §3.1：内核去掉相对路径与已在工作区根里的；重复的建会话回原会话，它们与偏好都不起作用）。
    async fn create(
        &mut self,
        persistence: Persistence,
        directories: Vec<rust_cli_kernel_api::env::EnvPath>,
    ) -> Outcome {
        let kernel = match self.kernel().await {
            Ok(kernel) => kernel,
            Err(message) => return Outcome::Refused(failed(message)),
        };
        // 会话偏好进新会话的起始状态（§48）：建之前问 Host 一次，这期间这个会话的命令排在通道里
        let preferences =
            crate::host_services::session_preferences(&self.parts.out, self.parts.sid.as_str())
                .await;
        let request = CreateSession {
            id: Some(self.parts.sid.clone()),
            workspace: self.parts.workspaces.workspace(&self.parts.key),
            selection: kernel.selection.clone(),
            mode: kernel.mode,
            plan: false,
            persistence,
            additional_directories: directories,
            preferences,
        };
        if let Err(error) = kernel
            .kernel
            .create_session_as(request, StartSource::Startup)
            .await
        {
            return Outcome::Created(Err(error));
        }
        // 会话建在这一套里：从这里起钉住它（之后的打开失败了，重试也回到这一套）
        self.kernel = Some(kernel.clone());
        self.draft = persistence == Persistence::Deferred;
        // 在建它的那一套里打开（两次取内核之间工作区的内核可能被拿走了，§72.9）
        match self.open_in(kernel, false).await {
            Ok(_) => Outcome::Created(Ok(())),
            Err(rejection) => Outcome::Refused(rejection),
        }
    }
}

/// 内核说找不到 → `precondition.notFound`；别的失败是内部错误。
pub(crate) fn not_found(error: KernelError) -> Rejection {
    match error {
        KernelError::NotFound { .. } => rejection(codes::PRECONDITION_NOT_FOUND, error.to_string()),
        other => failed(other.to_string()),
    }
}

#[cfg(test)]
#[path = "dirs_tests.rs"]
mod dirs_tests;
