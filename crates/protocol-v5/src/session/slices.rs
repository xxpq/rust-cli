//! `rows.range` 碰到骨架行时按片重建（protocol-v5.md §71.2）：只从库里读这一页的骨架行所在的存储消息（有回合的整回合一起），用同一组建投影的
//! 函数只建这一片，取这些 id 的整行；`v` 与 `createdAtSeq` 用 Publisher 索引里的（客户端手里的是这一份）。以前每一页都整份冷读、整份建投影
//! （§67.0 第 4 条），翻完一个长会话是平方级的。
//!
//! ```text
//! actor（同步）：page 里的骨架行 ─► 定位（有回合的用回合头的行号）─► walk ─► 这几条存储的消息 + 视图的底子
//! 任务：StoreQueries::history ─► 片的视图 ─► Projection::build ─► 取这些 id 的整行 ─► Event::SessionRead
//! ```

use std::collections::BTreeSet;
use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::{HistorySlice, SessionMeta, StoreQueries};
use serde_json::Value;

use super::projection::{Origin, Projection};
use super::queries::{fault, ok, range_result};
use super::rows::{CREATED_AT_SEQ, ROWS, keys};
use crate::errors::Fault;
use crate::generated::contract::codes;
use crate::generated::core_v1::Cursor;
use crate::publisher::{ID_KEY, Page, Publisher, VERSION_KEY};

/// 一页要重建的那一片：要读的存储消息（按走的先后），与建它要的视图的底子（不带消息）。
#[derive(Debug)]
pub(super) struct Slice {
    records: Vec<MessageId>,
    base: SessionView,
}

/// 这一页的片（在 actor 的这一步里算）。有回合的骨架行用它的回合头的行号定位（回合头留整行，§67.0 第 2 条），没有回合的用自己的行号。
pub(super) fn of(
    projection: &Projection,
    publisher: &Publisher,
    page: &Page,
    meta: SessionMeta,
) -> Slice {
    let outlined: BTreeSet<&str> = page.outlined.iter().map(String::as_str).collect();
    let seq = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or_default();
    let rows = page
        .items
        .iter()
        .filter(|item| outlined.contains(item[ID_KEY].as_str().unwrap_or_default()))
        .map(|item| {
            let turn = item[keys::TURN_ID].as_str().unwrap_or_default();
            let anchor = match turn {
                "" => seq(item.get(CREATED_AT_SEQ)),
                turn => seq(publisher.field(ROWS, turn, CREATED_AT_SEQ)),
            };
            (anchor, turn)
        });
    Slice {
        records: projection.walk().records(rows),
        base: projection.bare_view(meta),
    }
}

/// 片的视图里会话的身份（行不读它，`bare_view` 只按投影改种类）。
pub(super) fn bare_meta(
    sid: &str,
    workspace: rust_cli_kernel_api::env::WorkspaceRef,
) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(sid),
        kind: rust_cli_kernel_api::store::SessionKind::Interactive,
        parent: None,
        workspace,
        title: String::new(),
        trace: rust_cli_kernel_api::ids::TraceId::new(sid),
        created_at_ms: 0,
    }
}

/// 冷读的来源（片不是活的会话：没有运行时、没有准入过；子代理行留整行，不从片里取）。
fn origin() -> Origin {
    Origin {
        live: false,
        draft: false,
        deleted: false,
        title_source: rust_cli_kernel_api::session_facts::TitleSource::Default,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        start: super::projection::Start::Whole,
    }
}

/// 在任务里读这一片、建这一片，答这一页（§71.2）。
pub(super) async fn rebuilt(
    queries: Arc<dyn StoreQueries>,
    sid: String,
    slice: Slice,
    page: Page,
    cursor: Cursor,
) -> Result<Value, Fault> {
    let history = queries
        .history(&SessionId::new(&sid), &slice.records)
        .await
        .map_err(|error| fault(codes::PRECONDITION_STORE_UNAVAILABLE, error.to_string()))?;
    let more = page.more;
    let items = rows_of(slice.base, history, page)?;
    Ok(ok(range_result(items, more), cursor))
}

/// 片建出的整行换进这一页：骨架行换成重建的整行，`id`、`v`、`createdAtSeq` 取索引里的。骨架只给不再变的行，库里一定有它；没有就是内部错误，
/// 不拿骨架凑数（§67.3）。
fn rows_of(base: SessionView, history: HistorySlice, page: Page) -> Result<Vec<Value>, Fault> {
    let view = SessionView {
        messages: history.messages,
        turns: history.turns,
        output_heads: history.output_heads,
        tool_times: history.tool_times,
        ..base
    };
    let (_, mut content) = Projection::build(&view, origin());
    drop(view);
    let mut stored = content.collections.remove(ROWS).unwrap_or_default();
    let outlined: BTreeSet<&str> = page.outlined.iter().map(String::as_str).collect();
    let mut items = Vec::with_capacity(page.items.len());
    for item in page.items {
        let id = item[ID_KEY].as_str().unwrap_or_default();
        if !outlined.contains(id) {
            items.push(item);
            continue;
        }
        let fields = stored.remove(id).ok_or_else(|| {
            fault(
                codes::INTERNAL_FAILED,
                format!("row {id} is not in the stored session"),
            )
        })?;
        // 整行取重建的；id、版本与行号取索引里的（client 手里的是这一份）
        let mut whole: serde_json::Map<String, Value> = fields.into_iter().collect();
        for key in [ID_KEY, VERSION_KEY, CREATED_AT_SEQ] {
            whole.insert(key.to_owned(), item[key].clone());
        }
        items.push(Value::Object(whole));
    }
    Ok(items)
}

#[cfg(test)]
#[path = "slices_tests.rs"]
mod tests;
