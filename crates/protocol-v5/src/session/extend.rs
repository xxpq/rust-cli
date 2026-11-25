//! `rows.range` 往前补（protocol-v5.md §73.3）：投影只覆盖历史的一段后缀，Publisher 里 `beforeId` 之前一行都没有、下沿之前还有历史时，在任务里读
//! 下沿之前的一段（`StoreQueries::range`，补到回合开头），用同一组建投影的函数建这一段，号往前编（接在已知最早一行之前），回到 actor 里挂进投影与
//! Publisher（不发这些行的增量，只发一批 `rowsWindow`）。
//!
//! ```text
//! actor（同步）：page 为空 & earlier ─► Request{floor, first} ─► 任务：range(Before{floor, limit}) ─► 段的视图 ─► Projection::build
//!                                                                 ─► 号平移到 first 之前 ─► Event::SessionExtended
//! actor：floor 还是发起时的 ─► 挂进去、发 rowsWindow、答这一段；否则丢掉、按现在的 Publisher 重答
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::{HistoryRange, HistorySpan, StoreError, StoreQueries};
use serde_json::Value;

use super::projection::walk::Walk;
use super::projection::{Origin, Projection};
use super::queries::fault;
use super::rows::{CREATED_AT_SEQ, ROWS};
use crate::errors::Fault;
use crate::generated::contract::codes;
use crate::publisher::{Fields, ID_KEY, VERSION_KEY};

/// 一次往前补：发起时投影的下沿与已知最早一行的号（回来时先比下沿），与建这一段要的视图的底子（不带消息）。
#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) floor: MessageId,
    first: u64,
    base: SessionView,
}

/// 读回来、建好、号已往前编的一段。
#[derive(Debug)]
pub(crate) struct Chunk {
    /// 发起时的下沿：回来时投影的下沿不是它就丢掉这一段。
    pub(crate) from: MessageId,
    /// 这一段的行（按号升序），号在 `first` 之前、连着。
    pub(crate) rows: Vec<(String, Fields)>,
    pub(crate) walk: Walk,
    /// 段的索引的行号要加的量（段从 0 起建）。
    pub(crate) shift: i64,
    pub(crate) records: Vec<InputRecord>,
    /// 这一段的段首（新的下沿）与它之前还有没有。
    pub(crate) floor: Option<MessageId>,
    pub(crate) earlier: bool,
}

/// 要往前补时的请求（下沿之前没有历史、或投影没有行时为 None）。
pub(crate) fn request(
    projection: &Projection,
    meta: rust_cli_kernel_api::store::SessionMeta,
) -> Option<Request> {
    if !projection.earlier() {
        return None;
    }
    Some(Request {
        floor: projection.floor()?.clone(),
        first: projection.first_number()?,
        base: projection.bare_view(meta),
    })
}

/// 在任务里读下沿之前的 `records` 条存储消息（补到回合开头）、建这一段、号往前编。
pub(crate) async fn read(
    queries: Arc<dyn StoreQueries>,
    sid: &str,
    request: Request,
    records: usize,
) -> Result<Chunk, Fault> {
    let span = HistorySpan::Before {
        floor: Some(request.floor.clone()),
        records,
    };
    let range = queries
        .range(&SessionId::new(sid), span)
        .await
        .map_err(|error| match error {
            // 下沿不在分支上了：别处改写了历史（§73.8），client 之后收到新快照
            StoreError::NotFound { .. } => fault(codes::PRECONDITION_NOT_FOUND, error.to_string()),
            error => fault(codes::PRECONDITION_STORE_UNAVAILABLE, error.to_string()),
        })?;
    Ok(chunk(request, range))
}

/// 一段的视图（同冷读的视图，只带这一段的消息与事实）、段里的子代理，与它之前还有没有（侧聊的起点在段里时没有：起点之前的不显示，§57.5）。
/// 往前补与目录（§73.5）共用。
pub(crate) struct Segment {
    pub(crate) view: SessionView,
    pub(crate) subagents: Vec<rust_cli_kernel_api::agents::StoredSubagent>,
    pub(crate) earlier: bool,
    /// 段首那条存储消息（显示顺序里的第一条）：再往前读一段时的 `floor`。
    pub(crate) floor: Option<MessageId>,
}

pub(crate) fn segment(base: SessionView, range: HistoryRange) -> Segment {
    let loaded = range.loaded;
    // 撤销过文件改动的回合：checkpoint 的提问去重（同 runtime 冷视图的换法）
    let mut reverted: Vec<MessageId> = Vec::new();
    for checkpoint in &loaded.reverted {
        if !reverted.contains(&checkpoint.prompt) {
            reverted.push(checkpoint.prompt.clone());
        }
    }
    let view = SessionView {
        messages: loaded.earlier.into_iter().chain(loaded.messages).collect(),
        turns: loaded.turns,
        inputs: loaded.inputs,
        sources: loaded.sources,
        output_heads: loaded.output_heads,
        tool_times: loaded.tool_times,
        plans: loaded.plans,
        reverted,
        ..base
    };
    let earlier = range.earlier && !super::side_chat::starts_in(&view);
    Segment {
        view,
        subagents: range.subagents,
        earlier,
        floor: range.floor,
    }
}

/// 一段建出的行（号已平移到 `first` 之前、按号升序）与建它的投影。
struct Built {
    rows: Vec<(String, Fields)>,
    projection: Projection,
    shift: i64,
    records: Vec<InputRecord>,
    earlier: bool,
}

/// 用同一组建投影的函数建这一段（从 0 起编），再把号平移到 `first` 之前（接在已知最早一行前面、连着）。
fn built(base: SessionView, first: u64, range: HistoryRange) -> Built {
    let Segment {
        view,
        subagents,
        earlier,
        ..
    } = segment(base, range);
    let origin = Origin {
        subagents,
        ..Origin::default()
    };
    let (projection, mut content) = Projection::build(&view, origin);
    let shift = i64::try_from(first).unwrap_or(i64::MAX)
        - i64::try_from(projection.next_row()).unwrap_or(i64::MAX);
    let mut rows: Vec<(u64, String, Fields)> = content
        .collections
        .remove(ROWS)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, mut fields)| {
            let number = fields
                .get(CREATED_AT_SEQ)
                .and_then(Value::as_u64)
                .unwrap_or_default()
                .saturating_add_signed(shift);
            fields.insert(CREATED_AT_SEQ.to_owned(), Value::from(number));
            (number, id, fields)
        })
        .collect();
    rows.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    Built {
        rows: rows
            .into_iter()
            .map(|(_, id, fields)| (id, fields))
            .collect(),
        projection,
        shift,
        records: view.inputs,
        earlier,
    }
}

/// 往前补的一段：建好、号已平移。
fn chunk(request: Request, range: HistoryRange) -> Chunk {
    let Built {
        rows,
        mut projection,
        shift,
        records,
        earlier,
    } = built(request.base, request.first, range);
    Chunk {
        from: request.floor,
        rows,
        walk: projection.take_walk(),
        shift,
        records,
        floor: projection.floor().cloned(),
        earlier,
    }
}

/// 下沿之前的计划行（§73.4）：库里调用过 ExitPlanMode 的回合（`Calling`）建出的行里筛出计划行，按号升序；号平移到 `first` 之前，只用来排序
/// （之后往前补到这一段时同一行的号可以不同：计划目录与时间线的行各是各的）。`v` 是答的那一刻的 seq。
pub(crate) async fn plans(
    queries: Arc<dyn StoreQueries>,
    sid: &str,
    request: Request,
    v: u64,
) -> Result<Vec<Value>, Fault> {
    let span = HistorySpan::Calling {
        tool: rust_cli_kernel_api::tool_names::EXIT_PLAN_MODE.to_owned(),
        before: Some(request.floor.clone()),
    };
    let range = queries
        .range(&SessionId::new(sid), span)
        .await
        .map_err(|error| match error {
            StoreError::NotFound { .. } => fault(codes::PRECONDITION_NOT_FOUND, error.to_string()),
            error => fault(codes::PRECONDITION_STORE_UNAVAILABLE, error.to_string()),
        })?;
    let rows = built(request.base, request.first, range).rows;
    Ok(items(&rows, v)
        .into_iter()
        .filter(super::queries::is_plan)
        .collect())
}

/// 这一段答成 `rows.range` 的一页（§73.3）：最后 `limit` 行（一页的上限同从 Publisher 答的），再按字节预算与回合开头切（`range_result`）；
/// 前面还有这一段的行、或段首之前还有历史时 `hasMore` 为真。`v` 是挂进 Publisher 时的版本。
pub(crate) fn page(chunk: &Chunk, limit: usize, v: u64) -> Value {
    let rows = &chunk.rows[chunk.rows.len().saturating_sub(limit)..];
    let cut = rows.len() < chunk.rows.len();
    super::queries::range_result(items(rows, v), cut || chunk.earlier)
}

/// 这一段的行写成快照里的写法（`{id, v, ...字段}`，按号升序）。
fn items(rows: &[(String, Fields)], v: u64) -> Vec<Value> {
    rows.iter()
        .map(|(id, fields)| {
            let mut item: serde_json::Map<String, Value> =
                BTreeMap::clone(fields).into_iter().collect();
            item.insert(ID_KEY.to_owned(), Value::from(id.as_str()));
            item.insert(VERSION_KEY.to_owned(), Value::from(v));
            Value::Object(item)
        })
        .collect()
}

// ---------- actor 里的接线（§73.3、§73.4）：发起任务、回来之后挂进投影 ----------

use super::queries::{Answered, PLANS_KEY, Request as Query, ok, range_result};
use crate::errors::to_rpc;
use crate::generated::core_v1::Cursor;
use crate::server::{Actor, Event};
use crate::topics::session_topic;
use serde_json::json;

/// 计划目录：Publisher 里的（最新的在前）接上下沿之前库里的（更早，也是最新的在前，§73.4）。
pub(super) async fn earlier_plans(
    queries: std::sync::Arc<dyn rust_cli_kernel_api::store::StoreQueries>,
    sid: &str,
    mut plans: Vec<Value>,
    request: Option<super::extend::Request>,
    cursor: Cursor,
) -> Result<Value, Fault> {
    if let Some(request) = request {
        let earlier = self::plans(queries, sid, request, cursor.seq).await?;
        plans.extend(earlier.into_iter().rev());
    }
    Ok(ok(json!({PLANS_KEY: plans}), cursor))
}

/// 补回来的一段挂进投影的结果（§73.3）。
enum Placed {
    /// 挂进去了：这一页的答案。
    Answered(Value),
    /// topic 不在了（释放、进保留）：原样交回这一段，直接答它。
    Gone(Chunk),
    /// 投影的下沿变了（别的请求先补了、改写历史）：丢掉这一段，按现在的 Publisher 重答。
    Moved,
}

/// 往前补的一段回来了（§73.3）：发起时的回复、查询与 cursor，读到的段。
#[derive(Debug)]
pub(crate) struct Extended {
    reply: Value,
    key: String,
    sid: String,
    projected: Answered,
    cursor: Cursor,
    chunk: Result<super::extend::Chunk, Fault>,
}

impl Actor {
    /// 计划目录补上下沿之前的（§73.4）：在任务里读库，结果经 [`Event::SessionRead`] 回来。
    pub(super) fn read_plans(
        &mut self,
        reply: &Value,
        sid: String,
        plans: Vec<Value>,
        request: Option<Request>,
        cursor: Cursor,
    ) -> Result<Option<Value>, Fault> {
        let Some((_, queries)) = self.sources() else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        self.sessions.reads += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(self.spawner.spawn("protocol-v5-plans", async move {
            let result = earlier_plans(queries, &sid, plans, request, cursor).await;
            let _ = events.send(Event::SessionRead { reply, result });
        }));
        Ok(None)
    }

    /// 物化了的会话往前补（§73.3）：在任务里读下沿之前的一段、建好，结果经 [`Event::SessionExtended`] 回来（与读视图的任务同一个在途计数）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn read_extended(
        &mut self,
        reply: &Value,
        key: String,
        sid: String,
        projected: Answered,
        request: Option<Request>,
        limit: usize,
        cursor: Cursor,
    ) -> Result<Option<Value>, Fault> {
        let (Some((_, queries)), Some(request)) = (self.sources(), request) else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        self.sessions.reads += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(self.spawner.spawn("protocol-v5-rows-extend", async move {
            let chunk = read(queries, &sid, request, limit).await;
            let _ = events.send(Event::SessionExtended(Box::new(Extended {
                reply,
                key,
                sid,
                projected,
                cursor,
                chunk,
            })));
        }));
        Ok(None)
    }

    /// 往前补的一段回来了（§73.3）：投影的下沿还是发起时的就挂进投影与 Publisher、发一批 `rowsWindow`、答这一段；下沿变了（别的请求先补了、改写历史）
    /// 就丢掉这一段，按现在的 Publisher 重答这一页；topic 不在了直接答这一段。
    pub(crate) async fn session_extended(&mut self, extended: Extended) {
        self.sessions.reads -= 1;
        let Extended {
            reply,
            key,
            sid,
            projected,
            cursor,
            chunk,
        } = extended;
        let limit = match &projected {
            Answered::Range { limit, .. } => *limit,
            _ => usize::MAX,
        };
        let result = match chunk {
            Err(error) => Err(error),
            Ok(chunk) => match self.extend_topic(&key, &sid, chunk, limit) {
                Placed::Answered(answered) => Ok(Some(answered)),
                Placed::Gone(chunk) => Ok(Some(ok(page(&chunk, limit, cursor.seq), cursor))),
                Placed::Moved => self
                    .answer_topic(&reply, &key, &sid, Query::Answered(projected), None)
                    .unwrap_or_else(|| Ok(Some(ok(range_result(Vec::new(), false), cursor)))),
            },
        };
        match result {
            Ok(Some(value)) => {
                self.peer.respond(&reply, &Ok(value)).await;
            }
            Ok(None) => {}
            Err(error) => {
                self.peer.respond(&reply, &Err(to_rpc(&error))).await;
            }
        }
        self.flush().await;
        self.finish_shutdown();
    }

    /// 把这一段挂进物化了的投影（§73.3）。
    fn extend_topic(&mut self, key: &str, sid: &str, chunk: Chunk, limit: usize) -> Placed {
        let topic = session_topic(key, sid);
        let (Some(publisher), Some(projection)) = (
            self.topics.get_mut(&topic),
            self.sessions
                .slots
                .get_mut(sid)
                .and_then(|slot| slot.projection.as_mut()),
        ) else {
            return Placed::Gone(chunk);
        };
        if projection.floor() != Some(&chunk.from) {
            return Placed::Moved;
        }
        let page = page(&chunk, limit, publisher.cursor().seq);
        let rows = chunk
            .rows
            .iter()
            .map(|(id, fields)| (id.clone(), fields.clone(), projection.resident(fields)))
            .collect();
        publisher.extend(ROWS, super::projection::residence::OUTLINE, rows);
        let batch = projection.prepend(chunk);
        if publisher.publish(batch) {
            self.outbox.mark_topic(&topic);
        }
        Placed::Answered(ok(page, publisher.cursor()))
    }
}
