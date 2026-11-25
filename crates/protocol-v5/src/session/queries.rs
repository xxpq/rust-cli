//! 会话的只读查询（protocol-v5.md §40、§43、§44；02 §2.3）：只需投影的 `rows.range`、`session.plans`，答案取自会话 topic 的 Publisher
//! （它持有全部行，窗口只裁快照）；`session.fileChanges` 在 Publisher 上判定回合头，明细（每个文件的 `patches`）经内核读这个回合的 artifact（§44）；
//! 要会话运行时的 `work.bashOutput`、`session.subagents` 与 actor 里的调试记录 `session.debug`（§43）。`at` 都按 Publisher 判。
//!
//! ```text
//! query ── 校验（工作区、会话、参数）──┬─ topic 已登记 ─► at 过期？─► 投影 / 调试记录：这一步里答 ─► 直接回复
//!                                     │                         ├─► 文件改动：回合头在这一步判定，明细的任务 ─► Event::SessionRead
//!                                     │                         └─► 会话运行时：交给通道的读（reads.rs）─► Event::SessionRead
//!                                     └─ 没有 ─► 读视图的任务：cold_view → 临时投影（新 epoch，不登记）→ 同一个「答」（冷的答案；文件改动接着读明细）
//!                                                 ─► Event::SessionRead ─► 回复
//! ```
//!
//! actor 不等读库、不等内核（§4.1）：没有物化的会话在任务里读，文件改动的明细与会话运行时的读在任务里，结果是发给 actor 的消息。

use rust_cli_kernel_api::ids::{MessageId, SessionId, TurnId};
use rust_cli_kernel_api::session_api::{Kernel, TurnFileDiff};
use rust_cli_kernel_api::tool_names::EXIT_PLAN_MODE;
use serde_json::{Value, json};

use super::debug::DebugLog;
use super::lane::{LaneRequest, cold_view, not_found};
use super::projection::Projection;
use super::reads::Read;
use super::rows::{self, CREATED_AT_SEQ, ROWS, keys, kinds, tool_status};
use super::slices;
use crate::commands::field_path;
use crate::consts::{FILE_PATCHES_MAX_BYTES, SESSION_SNAPSHOT_BYTES};
use crate::errors::{Fault, protocol_error, to_rpc};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    Cursor, QueryEnvelope, QueryResult, QueryResultOk, QueryResultStale,
};
use crate::generated::session_rewind_v1::QuerySessionFileRewindPreviewParams;
use crate::generated::session_v1::{
    QueryRowsRangeParams, QuerySessionFileChangesParams, QuerySessionFileChangesResult,
    QuerySessionSubagentsParams, QueryToolOutputParams, QueryWorkBashOutputParams,
    RowTurnHeaderFileChangeSummaryState,
};
use crate::publisher::{Page, Publisher, start_at_opening};
use crate::server::{Actor, Event};
use crate::topics::{local_workspace, session_topic};

/// 查询的类型（session@1 的 `SESSION_QUERIES`）。
const ROWS_RANGE: &str = "rows.range";
const SESSION_PLANS: &str = "session.plans";
const SESSION_FILE_CHANGES: &str = "session.fileChanges";
const WORK_BASH_OUTPUT: &str = "work.bashOutput";
const SESSION_SUBAGENTS: &str = "session.subagents";
const SESSION_DEBUG: &str = "session.debug";
const TOOL_OUTPUT: &str = "tool.output";

/// 临时投影的 epoch 是 `<进程 epoch><这一段><序号>`（§40.2）：topic 用进程 epoch 本身，所以它不可能等于任何 topic 的 epoch。
const READ_EPOCH: &str = ":read:";

/// 结果里的键（`RowsRangeResultSchema` 与 `session.plans` 的 result）：行原样是快照里的写法（`{id, v, ...字段}`），不经生成的类型。
const ROWS_KEY: &str = "rows";
const HAS_MORE_KEY: &str = "hasMore";
pub(super) const PLANS_KEY: &str = "plans";

/// 会话核心里的六条查询（`session` 能力，§15.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionQuery {
    Range,
    Plans,
    FileChanges,
    BashOutput,
    Subagents,
    Debug,
    /// 回退文件的预览（`session.rewind` 能力，§53.3）。
    FileRewindPreview,
    /// 行上只有预览的工具输出的全文（01 §19.5，§68.0 第 7 条）。
    ToolOutput,
    /// 问题导航的目录（§73.5）。
    Prompts,
}

impl SessionQuery {
    pub(crate) fn of(kind: &str) -> Option<Self> {
        match kind {
            ROWS_RANGE => Some(Self::Range),
            SESSION_PLANS => Some(Self::Plans),
            SESSION_FILE_CHANGES => Some(Self::FileChanges),
            WORK_BASH_OUTPUT => Some(Self::BashOutput),
            SESSION_SUBAGENTS => Some(Self::Subagents),
            SESSION_DEBUG => Some(Self::Debug),
            super::rewind::FILE_REWIND_PREVIEW => Some(Self::FileRewindPreview),
            TOOL_OUTPUT => Some(Self::ToolOutput),
            super::prompts::SESSION_PROMPTS => Some(Self::Prompts),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Range => ROWS_RANGE,
            Self::Plans => SESSION_PLANS,
            Self::FileChanges => SESSION_FILE_CHANGES,
            Self::BashOutput => WORK_BASH_OUTPUT,
            Self::Subagents => SESSION_SUBAGENTS,
            Self::Debug => SESSION_DEBUG,
            Self::FileRewindPreview => super::rewind::FILE_REWIND_PREVIEW,
            Self::ToolOutput => TOOL_OUTPUT,
            Self::Prompts => super::prompts::SESSION_PROMPTS,
        }
    }
}

/// 一条查询（参数已按契约校验）。
#[derive(Debug, Clone)]
pub(super) enum Request {
    /// 按投影或调试记录同一步答的。
    Answered(Answered),
    /// 要会话运行时（或库）的读，在任务里答（§43.2、§45.3）。
    Runtime(Read),
}

/// 校验过的查询：经投影答的，或问题导航的目录（只读库、不经投影，§73.5）。
#[derive(Debug, Clone)]
enum Parsed {
    Request(Request),
    Prompts { before: Option<String> },
}

/// 同一步答的查询。
#[derive(Debug, Clone)]
pub(super) enum Answered {
    Range {
        before: Option<String>,
        limit: usize,
    },
    Plans,
    FileChanges {
        row: String,
    },
    /// 开发者面板的调试记录（actor 里，§43.2）。
    Debug,
    /// 回退文件的预览（§53.3）：回合头在这一步判定，预览经内核读。
    FileRewindPreview {
        row: String,
    },
    /// 工具输出的全文：从会话库读（§68.0 第 7 条）。
    ToolOutput {
        row: String,
    },
}

pub(crate) fn fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

/// 契约里的上限已经校验过（`limit` ≤ 200、`endedLimit` ≤ 100），换成下标的长度不会溢出。
fn count(limit: u64) -> usize {
    usize::try_from(limit).unwrap_or(usize::MAX)
}

/// 参数按契约的 schema 校验（`Query.<type>.params`，带字段路径），再读成生成的类型。未知字段忽略（查询参数宽松，11 §11 第 3 项）。
fn request(kind: SessionQuery, params: Value) -> Result<Parsed, Fault> {
    let invalid = |message: String| fault(codes::PRECONDITION_INVALID_PAYLOAD, message);
    if let Some(schema) = super::schemas::query(kind.name())
        && let Err(error) = schema.validate(&params, "")
    {
        let (field, message) = field_path(&error);
        return Err(invalid(format!("{field}: {message}")));
    }
    let read = |error: serde_json::Error| invalid(error.to_string());
    Ok(Parsed::Request(match kind {
        SessionQuery::Range => {
            let params: QueryRowsRangeParams = serde_json::from_value(params).map_err(read)?;
            Request::Answered(Answered::Range {
                before: params.before_id,
                limit: count(params.limit),
            })
        }
        SessionQuery::Plans => Request::Answered(Answered::Plans),
        SessionQuery::FileChanges => {
            let params: QuerySessionFileChangesParams =
                serde_json::from_value(params).map_err(read)?;
            Request::Answered(Answered::FileChanges { row: params.row_id })
        }
        SessionQuery::BashOutput => {
            let params: QueryWorkBashOutputParams = serde_json::from_value(params).map_err(read)?;
            Request::Runtime(Read::BashOutput {
                work: params.work_id,
            })
        }
        SessionQuery::Subagents => {
            let params: QuerySessionSubagentsParams =
                serde_json::from_value(params).map_err(read)?;
            Request::Runtime(Read::Subagents {
                after: params.ended_cursor,
                limit: count(params.ended_limit),
            })
        }
        SessionQuery::Debug => Request::Answered(Answered::Debug),
        SessionQuery::FileRewindPreview => {
            let params: QuerySessionFileRewindPreviewParams =
                serde_json::from_value(params).map_err(read)?;
            Request::Answered(Answered::FileRewindPreview { row: params.row_id })
        }
        SessionQuery::ToolOutput => {
            let params: QueryToolOutputParams = serde_json::from_value(params).map_err(read)?;
            Request::Answered(Answered::ToolOutput { row: params.row_id })
        }
        SessionQuery::Prompts => {
            let params: crate::generated::session_v1::QuerySessionPromptsParams =
                serde_json::from_value(params).map_err(read)?;
            return Ok(Parsed::Prompts {
                before: params.before_id,
            });
        }
    }))
}

/// 计划目录里的一行（§40.2，照 v4 的条件）：有结局、带计划文字的 ExitPlanMode 工具行。
///
/// 修改（PROJ-FACTS，§42.1）：以前不看计划文字（行上没有它，照 v4 过滤会去掉每一行）；现在行的 `input` 带工具读到的计划，按 v4 的条件过滤。
pub(super) fn is_plan(row: &Value) -> bool {
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
    text(keys::KIND) == kinds::TOOL_CALL
        && text(keys::TOOL_NAME) == EXIT_PLAN_MODE
        && tool_status::ended(text(keys::STATUS))
        && rows::has_plan(row)
}

/// `ok{data, cursor}`。
pub(crate) fn ok(data: Value, cursor: Cursor) -> Value {
    json!(QueryResult::Ok(QueryResultOk {
        data,
        cursor: Some(cursor),
    }))
}

/// `at` 过期（§40.2）：`stale{cursor: Publisher 现在的}`；没有过期为 None。
fn stale(publisher: &Publisher, at: Option<&Cursor>) -> Option<Value> {
    at.filter(|at| !publisher.current(at)).map(|_| {
        json!(QueryResult::Stale(QueryResultStale {
            cursor: publisher.cursor()
        }))
    })
}

/// 「答」的结果（§40.2、§44.2、§53.3）：这一步就答完的；或者判定已做完、答案要经内核读的（cursor 是判定这一刻的）。
#[derive(Debug)]
enum Answer {
    Ready(Value),
    Kernel {
        read: KernelRead,
        cursor: Cursor,
    },
    /// 这一页里有只剩骨架的行（§67.0 第 4 条）：在任务里从存储重建它们。
    Rebuild {
        page: Page,
        cursor: Cursor,
    },
    /// 工具输出的全文（§68.0 第 7 条）：在任务里从会话库读。
    Stored {
        row: String,
        cursor: Cursor,
    },
    /// Publisher 里 `beforeId` 之前一行都没有、投影的下沿之前还有历史（§73.3）：在任务里读下沿之前的一段、往前补。
    Extend {
        limit: usize,
        cursor: Cursor,
    },
    /// 计划目录：Publisher 里的（最新的在前）之外，下沿之前还有历史时在任务里读库补上更早的（§73.4）。
    Plans {
        plans: Vec<Value>,
        cursor: Cursor,
    },
}

/// 判定在 Publisher 上做完、答案经内核读的查询：IO 在任务里，不在 actor 里（§4.1）。
#[derive(Debug)]
enum KernelRead {
    /// 回合头的文件改动的明细（§44.2）；`reverted`：回合头的文件改动已撤销（§53.5），答案带同一个状态。
    FileDiffs { turn: TurnId, reverted: bool },
    /// 回退文件的预览（§53.3）：回合头所在回合开头的输入；没有时是空的预览。
    RewindPreview { target: Option<MessageId> },
}

/// 查询指名的回合头（文件改动与回退文件的预览只认它）：不在 → notFound；不是回合头 → invalid。
fn turn_header<'a>(
    publisher: &'a Publisher,
    row: &str,
) -> Result<&'a crate::publisher::Fields, Fault> {
    let fields = publisher.entity(ROWS, row).ok_or_else(|| {
        fault(
            codes::PRECONDITION_NOT_FOUND,
            format!("no row {row} in this session"),
        )
    })?;
    if rows::text(fields, keys::KIND) != kinds::TURN_HEADER {
        return Err(fault(
            codes::PRECONDITION_INVALID,
            format!("row {row} is not a turn header"),
        ));
    }
    Ok(fields)
}

/// 按 Publisher 答一条查询（§40.2）：`at` 过期 → stale；否则 ok。两者都带 Publisher 现在的 cursor。调试记录这里给的是冷的答案（临时投影：
/// 没有物化的会话没有观察到请求，§43.2）。文件改动只在这里判定回合头（§44.2）。
fn answer(
    publisher: &Publisher,
    projection: &Projection,
    request: &Answered,
    at: Option<&Cursor>,
    session: &str,
) -> Result<Answer, Fault> {
    if let Some(stale) = stale(publisher, at) {
        return Ok(Answer::Ready(stale));
    }
    let data = match request {
        Answered::Debug => DebugLog::default().snapshot(session),
        Answered::Range { before, limit } => {
            let mut page = publisher
                .page(ROWS, CREATED_AT_SEQ, before.as_deref(), *limit)
                .ok_or_else(|| {
                    fault(
                        codes::PRECONDITION_NOT_FOUND,
                        format!("no row {before:?} in this session"),
                    )
                })?;
            // 投影只覆盖一段后缀（§73.3）：之前一行都没有时往前补一段；之前还有几行时先答它们，下一页再补
            if page.items.is_empty() && projection.earlier() {
                let cursor = publisher.cursor();
                return Ok(Answer::Extend {
                    limit: *limit,
                    cursor,
                });
            }
            page.more |= projection.earlier();
            if !page.outlined.is_empty() {
                let cursor = publisher.cursor();
                return Ok(Answer::Rebuild { page, cursor });
            }
            range_result(page.items, page.more)
        }
        Answered::Plans => {
            let rows = publisher
                .page(ROWS, CREATED_AT_SEQ, None, usize::MAX)
                .map(|page| page.items)
                .unwrap_or_default();
            // 最新的在前（§40.2）
            let plans: Vec<Value> = rows.into_iter().rev().filter(is_plan).collect();
            if projection.earlier() {
                let cursor = publisher.cursor();
                return Ok(Answer::Plans { plans, cursor });
            }
            json!({PLANS_KEY: plans})
        }
        Answered::FileChanges { row } => {
            let fields = turn_header(publisher, row)?;
            // 明细经内核读这个回合的 artifact（§44.2）：IO 不在这里
            let read = KernelRead::FileDiffs {
                turn: TurnId::new(rows::text(fields, keys::TURN_ID)),
                reverted: rows::reverted(fields),
            };
            let cursor = publisher.cursor();
            return Ok(Answer::Kernel { read, cursor });
        }
        Answered::FileRewindPreview { row } => {
            turn_header(publisher, row)?;
            // 预览经内核读 checkpoint 与现在的文件（§53.3）：范围同 `files.rewind`，从这一回合开头的输入起
            let read = KernelRead::RewindPreview {
                target: super::rewind::opening_input(publisher, row).map(MessageId::new),
            };
            let cursor = publisher.cursor();
            return Ok(Answer::Kernel { read, cursor });
        }
        Answered::ToolOutput { row } => {
            let cursor = publisher.cursor();
            return Ok(Answer::Stored {
                row: row.clone(),
                cursor,
            });
        }
    };
    Ok(Answer::Ready(ok(data, publisher.cursor())))
}

/// 明细换成契约的形状（§44.2）：数字取回合记录的那一项；`patches` 按 items 的顺序放，这个文件的 hunk 的 JSON 字节加上已放的不超过
/// `budget` 才放，否则这个文件为空、接着看下一个（回复是一条链路消息，不能超过单条上限）。
fn file_changes(diffs: &[TurnFileDiff], budget: usize) -> QuerySessionFileChangesResult {
    let mut used = 0;
    let items: Vec<_> = diffs
        .iter()
        .map(|diff| {
            let patches: Vec<_> = diff.hunks.iter().map(rows::file_patch).collect();
            let bytes = json!(patches).to_string().len();
            let fits = used + bytes <= budget;
            if fits {
                used += bytes;
            }
            rows::file_item(&diff.file, if fits { patches } else { Vec::new() })
        })
        .collect();
    rows::file_changes(&items)
}

/// 回合头的文件改动的明细（§44.2）：经内核读这个回合的 diff（在任务里）。
/// 经内核读的答案（[`KernelRead`]）。
async fn kernel_read(
    kernel: &dyn Kernel,
    sid: &str,
    read: KernelRead,
    cursor: Cursor,
) -> Result<Value, Fault> {
    match read {
        KernelRead::FileDiffs { turn, reverted } => {
            file_diffs(kernel, sid, &turn, reverted, cursor).await
        }
        KernelRead::RewindPreview { target } => {
            super::rewind::preview(kernel, sid, target, cursor).await
        }
    }
}

async fn file_diffs(
    kernel: &dyn Kernel,
    sid: &str,
    turn: &TurnId,
    reverted: bool,
    cursor: Cursor,
) -> Result<Value, Fault> {
    let diffs = kernel
        .turn_file_diffs(&SessionId::new(sid), turn)
        .await
        .map_err(|error| {
            let rejection = not_found(error);
            fault(rejection.code, rejection.message)
        })?;
    let mut result = file_changes(&diffs, FILE_PATCHES_MAX_BYTES);
    // 撤销过的回合（§53.5）：明细照给（同 v4），状态与回合头同一个
    if reverted && result.state.is_some() {
        result.state = Some(RowTurnHeaderFileChangeSummaryState::Reverted);
    }
    Ok(ok(json!(result), cursor))
}

/// `rows.range` 的答案：一页也有字节预算（01 §19.5）：从尾往前留，累计的行字节不超过 [`SESSION_SNAPSHOT_BYTES`]，至少一行；再从留下的行里的
/// 第一个回合开头起（protocol-v5.md §71.1）。截掉了前面的行时 `hasMore` 为真（client 接着按留下的第一行往前翻）。
pub(super) fn range_result(mut items: Vec<Value>, more: bool) -> Value {
    let mut bytes = 0;
    let mut keep = items.len();
    for (index, item) in items.iter().enumerate().rev() {
        let size = serde_json::to_vec(item).map_or(0, |text| text.len());
        if keep < items.len() && bytes + size > SESSION_SNAPSHOT_BYTES {
            break;
        }
        bytes += size;
        keep = index;
    }
    let cut = keep > 0;
    items.drain(..keep);
    let opened = start_at_opening(&mut items, rows::opens);
    json!({ROWS_KEY: items, HAS_MORE_KEY: more || cut || opened})
}

/// 会话查询的外壳（§40.2）：工作区必填且是本机的绝对路径、会话必填。回改写历史的预览也用它（§53.3）。
pub(super) fn target(query: &QueryEnvelope, name: &str) -> Result<(String, String), Fault> {
    let Some(key) = query.target.workspace_key.clone() else {
        return Err(fault(
            codes::PRECONDITION_WORKSPACE_REQUIRED,
            format!("{name} needs target.workspaceKey"),
        ));
    };
    local_workspace(&key).map_err(|message| fault(codes::PRECONDITION_INVALID, message))?;
    let Some(sid) = query
        .target
        .session_id
        .clone()
        .filter(|sid| !sid.is_empty())
    else {
        return Err(fault(
            codes::PRECONDITION_INVALID_PAYLOAD,
            format!("{name} needs target.sessionId"),
        ));
    };
    Ok((key, sid))
}

/// 临时投影的 epoch（§40.2）：进程 epoch 加 [`READ_EPOCH`] 加序号，不可能等于任何 topic 的 epoch。
pub(super) fn read_epoch(process: &str, temporary: u64) -> String {
    format!("{process}{READ_EPOCH}{temporary}")
}

impl Actor {
    /// 一条会话查询（§40.2、§43.2）。topic 已登记的：投影与调试记录在这一步里答（返回结果，直接回复），会话运行时的读交给通道；没有登记的
    /// 交给读视图的任务。后两种返回 None，回复等 [`Event::SessionRead`]。
    pub(crate) fn session_query(
        &mut self,
        reply: &Value,
        kind: SessionQuery,
        query: QueryEnvelope,
    ) -> Result<Option<Value>, Fault> {
        let (key, sid) = target(&query, kind.name())?;
        let request = request(kind, query.params.unwrap_or(Value::Null))?;
        if self.other_workspace(&key, &sid) {
            return Err(fault(
                codes::PRECONDITION_WORKSPACE_MISMATCH,
                format!("the session {sid} belongs to another workspace"),
            ));
        }
        let topic = session_topic(&key, &sid);
        let request = match request {
            // 目录只读库、不经投影（§73.5）：物化与否同一条路
            Parsed::Prompts { before } => return self.read_prompts(reply, key, sid, before),
            Parsed::Request(request) => request,
        };
        if let Some(answered) =
            self.answer_topic(reply, &key, &sid, request.clone(), query.at.as_ref())
        {
            return answered;
        }
        self.read_unmaterialized(reply, key, sid, topic, request, query.at)
    }

    /// 物化了的会话的一条查询（topic 与投影同时登记、同时注销，§66）：这一步答，或交给任务（结果经事件回来）。没有物化时为 None。
    pub(super) fn answer_topic(
        &mut self,
        reply: &Value,
        key: &str,
        sid: &str,
        request: Request,
        at: Option<&Cursor>,
    ) -> Option<Result<Option<Value>, Fault>> {
        let (key, sid) = (key.to_owned(), sid.to_owned());
        let topic = session_topic(&key, &sid);
        let materialized = self.topics.get(&topic).zip(
            self.sessions
                .slots
                .get(&sid)
                .and_then(|slot| slot.projection.as_ref()),
        );
        let (publisher, projection) = materialized?;
        if let Some(stale) = stale(publisher, at) {
            return Some(Ok(Some(stale)));
        }
        let cursor = publisher.cursor();
        Some(match request {
            Request::Answered(Answered::Debug) => Ok(Some(ok(self.sessions.debug(&sid), cursor))),
            // 冷读建、没被命令改过的投影（§70.0 第 1 条）：没有会话运行时，库里的子代理物化时读过，这一步答，不读第二遍。投影只覆盖一段后缀时
            // 它的那份只有段里的 Agent 调用（§73.4），经通道读库
            Request::Runtime(read) => match self
                .sessions
                .cold_subagents(&sid)
                .filter(|_| !projection.earlier())
            {
                Some(stored) => read.answer_cold(stored).map(|data| Some(ok(data, cursor))),
                None => self.read_runtime(reply, &key, &sid, read, cursor),
            },
            Request::Answered(projected) => {
                match answer(publisher, projection, &projected, None, &sid) {
                    Err(error) => Err(error),
                    Ok(Answer::Ready(value)) => Ok(Some(value)),
                    Ok(Answer::Kernel { read, cursor }) => {
                        self.read_kernel(reply, key, sid, read, cursor)
                    }
                    Ok(Answer::Rebuild { page, cursor }) => {
                        let slice = self.sources().map(|(workspaces, _)| {
                            let meta = slices::bare_meta(&sid, workspaces.workspace(&key));
                            slices::of(projection, publisher, &page, meta)
                        });
                        self.read_rebuilt(reply, sid, slice, page, cursor)
                    }
                    Ok(Answer::Stored { row, cursor }) => {
                        self.read_stored(reply, key, sid, row, cursor)
                    }
                    Ok(Answer::Extend { limit, cursor }) => {
                        let request = self.sources().and_then(|(workspaces, _)| {
                            let meta = slices::bare_meta(&sid, workspaces.workspace(&key));
                            super::extend::request(projection, meta)
                        });
                        self.read_extended(reply, key, sid, projected, request, limit, cursor)
                    }
                    Ok(Answer::Plans { plans, cursor }) => {
                        let request = self.sources().and_then(|(workspaces, _)| {
                            let meta = slices::bare_meta(&sid, workspaces.workspace(&key));
                            super::extend::request(projection, meta)
                        });
                        self.read_plans(reply, sid, plans, request, cursor)
                    }
                }
            }
        })
    }

    /// 没有物化的会话的一条查询（§40.2）：在任务里冷读、建临时投影、答完丢掉。
    fn read_unmaterialized(
        &mut self,
        reply: &Value,
        key: String,
        sid: String,
        topic: String,
        request: Request,
        at: Option<Cursor>,
    ) -> Result<Option<Value>, Fault> {
        let Some((workspaces, queries)) = self.sources() else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        self.sessions.temporary += 1;
        let epoch = read_epoch(&self.epoch, self.sessions.temporary);
        self.sessions.reads += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(self.spawner.spawn("protocol-v5-session-read", async move {
            let read = cold_view(
                workspaces.clone(),
                queries.clone(),
                key.clone(),
                SessionId::new(&sid),
            )
            .await;
            let result = match read {
                Ok((kernel, view, origin)) => {
                    // 临时投影：不登记 topic，答完随任务丢掉（02 §2.3 的查询零副作用）
                    let (projection, content) = Projection::build(&view, origin);
                    let mut publisher = Publisher::new(topic, epoch);
                    publisher.adopt(content);
                    match request {
                        // 没有会话运行时：后台输出不可用；子代理目录用这次冷读已经读出的那份（§43.2、§70.0 第 1 条：以前在这里又读一遍库）；
                        // 冷读只读了一段后缀时那份只有段里的，读库（§73.4）
                        Request::Runtime(read) => match stale(&publisher, at.as_ref()) {
                            Some(stale) => Ok(stale),
                            None => {
                                let stored = if projection.earlier() {
                                    super::reads::stored_subagents(
                                        queries.as_ref(),
                                        &SessionId::new(&sid),
                                    )
                                    .await
                                    .map_err(|error| {
                                        fault(
                                            codes::PRECONDITION_STORE_UNAVAILABLE,
                                            error.to_string(),
                                        )
                                    })
                                } else {
                                    Ok(projection.stored_subagents())
                                };
                                stored.and_then(|stored| {
                                    read.answer_cold(stored)
                                        .map(|data| ok(data, publisher.cursor()))
                                })
                            }
                        },
                        Request::Answered(projected) => {
                            match answer(&publisher, &projection, &projected, at.as_ref(), &sid) {
                                Ok(Answer::Ready(value)) => Ok(value),
                                Ok(Answer::Kernel { read, cursor }) => {
                                    kernel_read(kernel.kernel.as_ref(), &sid, read, cursor).await
                                }
                                Ok(Answer::Rebuild { page, cursor }) => {
                                    let slice = slices::of(
                                        &projection,
                                        &publisher,
                                        &page,
                                        view.meta.clone(),
                                    );
                                    slices::rebuilt(queries, sid, slice, page, cursor).await
                                }
                                Ok(Answer::Stored { row, cursor }) => {
                                    super::tool_output::read(
                                        workspaces, queries, key, sid, row, cursor,
                                    )
                                    .await
                                }
                                // 临时投影往前补（§73.3）：补完直接答这一段，投影随任务丢掉
                                Ok(Answer::Extend { limit, cursor }) => {
                                    match super::extend::request(&projection, view.meta.clone()) {
                                        Some(request) => {
                                            super::extend::read(queries, &sid, request, limit)
                                                .await
                                                .map(|chunk| {
                                                    let page = super::extend::page(
                                                        &chunk, limit, cursor.seq,
                                                    );
                                                    ok(page, cursor)
                                                })
                                        }
                                        None => Ok(ok(range_result(Vec::new(), false), cursor)),
                                    }
                                }
                                Ok(Answer::Plans { plans, cursor }) => {
                                    let request =
                                        super::extend::request(&projection, view.meta.clone());
                                    super::extend::earlier_plans(
                                        queries, &sid, plans, request, cursor,
                                    )
                                    .await
                                }
                                Err(error) => Err(error),
                            }
                        }
                    }
                }
                Err(rejection) => Err(fault(rejection.code, rejection.message)),
            };
            let _ = events.send(Event::SessionRead { reply, result });
        }));
        Ok(None)
    }

    /// `rows.range` 的一页里有骨架（§67.0 第 4 条）：交给任务按片重建（§71.2，只读这一页的消息），结果经 [`Event::SessionRead`] 回来
    /// （与读视图的任务同一个在途计数）。`slice` 是这一步里按投影的索引算好的（进程级部件没建好时为 None）。
    fn read_rebuilt(
        &mut self,
        reply: &Value,
        sid: String,
        slice: Option<slices::Slice>,
        page: Page,
        cursor: Cursor,
    ) -> Result<Option<Value>, Fault> {
        let (Some((_, queries)), Some(slice)) = (self.sources(), slice) else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        self.sessions.reads += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(self.spawner.spawn("protocol-v5-rows-rebuild", async move {
            let result = slices::rebuilt(queries, sid, slice, page, cursor).await;
            let _ = events.send(Event::SessionRead { reply, result });
        }));
        Ok(None)
    }

    /// 工具输出的全文（§68.0 第 7 条）：交给任务从会话库读，结果经 [`Event::SessionRead`] 回来（与读视图的任务同一个在途计数）。
    fn read_stored(
        &mut self,
        reply: &Value,
        key: String,
        sid: String,
        row: String,
        cursor: Cursor,
    ) -> Result<Option<Value>, Fault> {
        let Some((workspaces, queries)) = self.sources() else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        self.sessions.reads += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(self.spawner.spawn("protocol-v5-tool-output", async move {
            let result = super::tool_output::read(workspaces, queries, key, sid, row, cursor).await;
            let _ = events.send(Event::SessionRead { reply, result });
        }));
        Ok(None)
    }

    /// 要会话运行时的读（§43.2）：交给这个会话的通道（同一会话的请求按到达顺序），通道有 handle 时起读的任务，没有时给冷的答案；结果都经
    /// [`Event::RuntimeRead`] 回来。在途时表项记着它（挡着热会话的回收，§69.3）。
    fn read_runtime(
        &mut self,
        reply: &Value,
        key: &str,
        sid: &str,
        read: Read,
        cursor: Cursor,
    ) -> Result<Option<Value>, Fault> {
        let Some(requests) = self.lane(key, sid) else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        let query = LaneRequest::Query {
            reply: reply.clone(),
            read,
            cursor,
        };
        if requests.send(query).is_ok() {
            self.sessions.reads += 1;
            if let Some(slot) = self.sessions.slots.get_mut(sid) {
                slot.runtime_reads += 1;
            }
        }
        Ok(None)
    }

    /// 物化了的会话经内核读的答案（文件改动的明细 §44.2、回退文件的预览 §53.3）：判定已在这一步做完，交给任务经内核读，结果经
    /// [`Event::SessionRead`] 回来（与读视图的任务同一个在途计数，`shutdown` 等它们）。
    fn read_kernel(
        &mut self,
        reply: &Value,
        key: String,
        sid: String,
        read: KernelRead,
        cursor: Cursor,
    ) -> Result<Option<Value>, Fault> {
        let Some((workspaces, _)) = self.sources() else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        self.sessions.reads += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(self.spawner.spawn("protocol-v5-kernel-read", async move {
            let result = match workspaces.open(&key).await {
                Ok(kernel) => kernel_read(kernel.kernel.as_ref(), &sid, read, cursor).await,
                Err(message) => Err(fault(codes::INTERNAL_FAILED, message)),
            };
            let _ = events.send(Event::SessionRead { reply, result });
        }));
        Ok(None)
    }

    /// 读视图的任务回来了：直接写出回复（同 `sessions.facts`：查询的结果与帧不保证先后，§40.4）。
    pub(crate) async fn session_read(&mut self, reply: Value, result: Result<Value, Fault>) {
        self.sessions.reads -= 1;
        self.peer
            .respond(&reply, &result.map_err(|error| to_rpc(&error)))
            .await;
        self.finish_shutdown();
    }
}

#[cfg(test)]
#[path = "queries_tests.rs"]
mod tests;
