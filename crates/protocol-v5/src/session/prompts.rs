//! 问题导航的目录 `session.prompts`（protocol-v5.md §73.5）：按存储从尾往前一段一段给每条真实用户输入（行 id、回合、开头的文字）与那一轮回答的开头。
//! 不经 Publisher、不改投影的下沿：读一段（`StoreQueries::range`，补到回合开头），用同一组建投影的函数建一份临时投影，取出用户输入行与回答行，丢掉。
//!
//! ```text
//! 任务：range(Before{beforeId, PROMPTS_PAGE_RECORDS}) ─► 段的视图 ─► Projection::build ─► 按号排好的行 ─► 提问与回答 ─► Event::SessionRead
//! ```

use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::{HistorySpan, StoreError, StoreQueries};
use serde_json::{Value, json};

use super::extend::{Segment, segment};
use super::projection::{Origin, Projection};
use super::queries::fault;
use super::rows::{self, CREATED_AT_SEQ, ROWS, keys, kinds};
use crate::consts::{PROMPT_ANSWER_CHARS, PROMPT_TEXT_CHARS, PROMPTS_PAGE_RECORDS};
use crate::errors::Fault;
use crate::generated::contract::codes;
use crate::generated::core_v1::{QueryResult, QueryResultOk};
use crate::generated::session_v1::{QuerySessionPromptsResult, QuerySessionPromptsResultItemsItem};
use crate::publisher::Fields;

/// 查询的类型（`session` 核心，§73.5）。
pub(crate) const SESSION_PROMPTS: &str = "session.prompts";

/// 在任务里读一页目录：`before`（存储消息 id）之前的 [`PROMPTS_PAGE_RECORDS`] 条存储消息；`selection` 与 `mode` 只为组视图（行不读它们）。
pub(crate) async fn page(
    queries: Arc<dyn StoreQueries>,
    sid: &str,
    before: Option<String>,
    (selection, mode): (ModelSelection, PermissionMode),
) -> Result<Value, Fault> {
    let span = HistorySpan::Before {
        floor: before.map(MessageId::new),
        records: PROMPTS_PAGE_RECORDS,
    };
    let range = queries
        .range(&SessionId::new(sid), span)
        .await
        .map_err(|error| match error {
            StoreError::NotFound { .. } => fault(codes::PRECONDITION_NOT_FOUND, error.to_string()),
            error => fault(codes::PRECONDITION_STORE_UNAVAILABLE, error.to_string()),
        })?;
    let base = SessionView::new(range.loaded.meta.clone(), Vec::new(), selection, mode);
    let Segment {
        view,
        earlier,
        floor,
        ..
    } = segment(base, range);
    let floor = floor
        .filter(|_| earlier)
        .map(|floor| floor.as_str().to_owned());
    let (_, mut content) = Projection::build(&view, Origin::default());
    let mut built: Vec<(String, Fields)> = content
        .collections
        .remove(ROWS)
        .unwrap_or_default()
        .into_iter()
        .collect();
    built.sort_by_key(|(_, fields)| fields.get(CREATED_AT_SEQ).and_then(Value::as_u64));
    let result = QuerySessionPromptsResult {
        items: prompts(&built),
        before: floor,
    };
    Ok(json!(result))
}

/// 按显示顺序排好的行 → 目录项：每条真实用户输入一项；有回合的，回答是这个回合里的正文行；不属于回合的，回答是它之后、下一条用户输入之前的
/// 正文行（§73.5）。
fn prompts(built: &[(String, Fields)]) -> Vec<QuerySessionPromptsResultItemsItem> {
    fn kind(fields: &Fields) -> &str {
        rows::text(fields, keys::KIND)
    }
    let mut items = Vec::new();
    for (at, (id, fields)) in built.iter().enumerate() {
        if kind(fields) != kinds::USER_INPUT || !rows::real_user(fields) {
            continue;
        }
        let turn = rows::text(fields, keys::TURN_ID);
        let answers = |(_, row): &&(String, Fields)| kind(row) == kinds::ASSISTANT_TEXT;
        let texts: Vec<&str> = if turn.is_empty() {
            built[at + 1..]
                .iter()
                .take_while(|(_, row)| kind(row) != kinds::USER_INPUT)
                .filter(answers)
                .map(|(_, row)| rows::text(row, keys::TEXT))
                .collect()
        } else {
            built
                .iter()
                .filter(|(_, row)| rows::text(row, keys::TURN_ID) == turn)
                .filter(answers)
                .map(|(_, row)| rows::text(row, keys::TEXT))
                .collect()
        };
        items.push(QuerySessionPromptsResultItemsItem {
            row_id: id.clone(),
            turn_id: turn.to_owned(),
            text: rows::text(fields, keys::TEXT)
                .chars()
                .take(PROMPT_TEXT_CHARS)
                .collect(),
            answer: openings(&texts, PROMPT_ANSWER_CHARS),
        });
    }
    items
}

/// 回答正文行的开头：按先后取，合计至多 `budget` 个字符（最后一段截断，空的不要）。
fn openings(texts: &[&str], budget: usize) -> Vec<String> {
    let mut left = budget;
    let mut out = Vec::new();
    for text in texts {
        if left == 0 {
            break;
        }
        let taken: String = text.chars().take(left).collect();
        left -= taken.chars().count();
        if !taken.is_empty() {
            out.push(taken);
        }
    }
    out
}

// ---------- actor 里的接线 ----------

impl crate::server::Actor {
    /// 问题导航的目录（§73.5）：在任务里读库的一段、建临时投影、取提问与回答，结果经 [`crate::server::Event::SessionRead`] 回来。视图的模型与模式取工作区的缺省
    /// （行不读它们）。
    pub(super) fn read_prompts(
        &mut self,
        reply: &Value,
        key: String,
        sid: String,
        before: Option<String>,
    ) -> Result<Option<Value>, Fault> {
        let Some((workspaces, queries)) = self.sources() else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the session store is not ready yet",
            ));
        };
        self.sessions.reads += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(self.spawner.spawn("protocol-v5-prompts", async move {
            let result = match workspaces.open(&key).await {
                Ok(kernel) => {
                    let defaults = (kernel.selection.clone(), kernel.mode);
                    page(queries, &sid, before, defaults).await
                }
                Err(message) => Err(fault(codes::INTERNAL_FAILED, message)),
            };
            // 不带 cursor：答案按库的「现在」，与投影的 epoch 无关（§73.5）
            let result =
                result.map(|data| json!(QueryResult::Ok(QueryResultOk { data, cursor: None })));
            let _ = events.send(crate::server::Event::SessionRead { reply, result });
        }));
        Ok(None)
    }
}

#[cfg(test)]
#[path = "prompts_tests.rs"]
mod tests;
