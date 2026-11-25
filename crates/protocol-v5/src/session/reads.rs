//! 要会话运行时的读（protocol-v5.md §43.2）：`work.bashOutput`、`session.subagents`。本进程打开着这个会话时，通道借它的 handle
//! 在读的任务里读；没有会话运行时（冷会话、没有物化的、通道没有打开会话）时是冷的答案。子代理目录另读库（§45.3）。这里只把内核的读换成
//! 契约的形状。

use std::sync::Arc;

use rust_cli_kernel_api::agents::{StoredCall, StoredSubagent};
use rust_cli_kernel_api::background::WorkOutput;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};
use rust_cli_kernel_api::session_api::{KernelError, SessionHandle};
use rust_cli_kernel_api::store::{StoreError, StoreQueries};
use serde_json::{Value, json};

use super::queries::fault;
use crate::consts::BASH_OUTPUT_TAIL_BYTES;
use crate::errors::Fault;
use crate::generated::contract::codes;
use crate::generated::session_v1::{
    QuerySessionSubagentsResult, QuerySessionSubagentsResultEnded,
    QuerySessionSubagentsResultEndedItemsItem, QuerySessionSubagentsResultEndedItemsItemStatus,
    QuerySessionSubagentsResultRunningItem, QuerySessionSubagentsResultRunningItemStatus,
    QueryWorkBashOutputResult, QueryWorkBashOutputResultAlt1, QueryWorkBashOutputResultAlt1Kind,
    QueryWorkBashOutputResultAlt1Status, QueryWorkBashOutputResultAlt2,
    QueryWorkBashOutputResultAlt2Kind,
};

/// 子代理没有说明时的标题（契约要非空；同 Node 的缺省）。
const SUBAGENT_TITLE: &str = "Subagent";
/// 库里的调用没有 agent 类型时（Node 写的、input 没有 `subagent_type`）的类型（同 Node 的缺省）；子代理行也用它（§54.1）。
pub(super) const SUBAGENT_TYPE: &str = "subagent";
/// 失败的调用的错误里有这些词就算停掉的（Node `/abort|cancel|interrupt|stop/i`）。
const CANCELLATION_WORDS: [&str; 4] = ["abort", "cancel", "interrupt", "stop"];
/// `endedCursor` 的字段名（不认识的 cursor 是参数错误）。
const ENDED_CURSOR: &str = "endedCursor";

/// 一条要会话运行时的读（参数已按契约校验）。
#[derive(Debug, Clone)]
pub(crate) enum Read {
    BashOutput {
        work: String,
    },
    Subagents {
        /// 上一页最后一项的子会话 id。
        after: Option<String>,
        limit: usize,
    },
}

fn failed(error: KernelError) -> Fault {
    fault(codes::INTERNAL_FAILED, error.to_string())
}

impl Read {
    /// 读的任务里答（§43.2、§45.3）：本进程打开着这个会话时带它的 handle，先读会话运行时；子代理目录再读库（冷会话、本进程之前派生的
    /// 子代理在库里）。没有 handle 时后台输出不可用（rust 的后台命令随进程结束，照 Node 不恢复冷会话）。
    pub(crate) async fn answer(
        self,
        handle: Option<Arc<dyn SessionHandle>>,
        queries: Arc<dyn StoreQueries>,
        session: SessionId,
    ) -> Result<Value, Fault> {
        match self {
            Self::BashOutput { work } => {
                let output = match handle {
                    Some(handle) => handle
                        .work_output(&work, BASH_OUTPUT_TAIL_BYTES)
                        .await
                        .map_err(failed)?,
                    None => None,
                };
                Ok(bash_output(&work, output))
            }
            Self::Subagents { after, limit } => {
                // 先读表、再读库（§45.3 的竞态）：表说已结束的，子会话最后的回答已经提交
                let live = match handle {
                    Some(handle) => handle.subagents().await.map_err(failed)?,
                    None => Vec::new(),
                };
                let stored = stored_subagents(queries.as_ref(), &session)
                    .await
                    .map_err(|error| failed(error.into()))?;
                subagents(live, stored, after.as_deref(), limit)
            }
        }
    }
}

impl Read {
    /// 没有会话运行时、库里的子代理已经读过时答（§70.0 第 1 条：冷读建的投影里的那份、临时投影那次冷读的那份）：后台输出不可用，
    /// 子代理目录是运行时的表为空时的那一份，与 [`Self::answer`] 没有 handle 时逐项相同，只是不再读库。
    pub(crate) fn answer_cold(self, stored: Vec<StoredSubagent>) -> Result<Value, Fault> {
        match self {
            Self::BashOutput { work } => Ok(bash_output(&work, None)),
            Self::Subagents { after, limit } => {
                subagents(Vec::new(), stored, after.as_deref(), limit)
            }
        }
    }
}

/// 库里的子代理（§45.3）：草稿（还没落库）与删掉的会话库里没有，是没有派生过子代理。子代理目录与子代理行的冷读（§54.3）都读它。
pub(super) async fn stored_subagents(
    queries: &dyn StoreQueries,
    session: &SessionId,
) -> Result<Vec<StoredSubagent>, StoreError> {
    match queries.subagents(session).await {
        Err(StoreError::NotFound { .. }) => Ok(Vec::new()),
        read => read,
    }
}

/// 后台命令的状态（内核的四种 → 契约的）：停掉的、被输出上限结束的都是 `cancelled`。
fn status(status: BackgroundStatus) -> QueryWorkBashOutputResultAlt1Status {
    match status {
        BackgroundStatus::Running => QueryWorkBashOutputResultAlt1Status::Running,
        BackgroundStatus::Completed => QueryWorkBashOutputResultAlt1Status::Completed,
        BackgroundStatus::Failed => QueryWorkBashOutputResultAlt1Status::Failed,
        BackgroundStatus::Killed => QueryWorkBashOutputResultAlt1Status::Cancelled,
    }
}

/// `work.bashOutput` 的结果（§43.2 的表）。
pub(crate) fn bash_output(work: &str, output: Option<WorkOutput>) -> Value {
    let unread = |kind, code: Option<&str>| {
        QueryWorkBashOutputResult::Alt2(QueryWorkBashOutputResultAlt2 {
            kind,
            work_id: work.to_owned(),
            code: code.map(str::to_owned),
        })
    };
    let result = match output {
        None => unread(QueryWorkBashOutputResultAlt2Kind::Unavailable, None),
        Some(WorkOutput {
            status: current,
            read: Ok(tail),
        }) => QueryWorkBashOutputResult::Alt1(QueryWorkBashOutputResultAlt1 {
            kind: QueryWorkBashOutputResultAlt1Kind::Output,
            work_id: work.to_owned(),
            status: status(current),
            output: tail.text,
            truncated: tail.truncated,
            output_path: tail.path.as_str().to_owned(),
        }),
        Some(WorkOutput { read: Err(why), .. }) => unread(
            QueryWorkBashOutputResultAlt2Kind::ReadFailed,
            Some(why.as_str()),
        ),
    };
    json!(result)
}

/// 目录里的一项（表与库按子会话合并之后，§45.3）。
struct Item {
    child: String,
    agent: Option<String>,
    tool_call: Option<String>,
    agent_type: String,
    title: String,
    started_at: Option<u64>,
    ended_at: Option<u64>,
    summary: Option<String>,
}

enum State {
    Running { waiting: bool },
    Ended(QuerySessionSubagentsResultEndedItemsItemStatus),
}

/// 表里的一项（本进程派生的）：状态与这次运行的时刻取表，toolCallId 与 summary 取库（启动的回执还没落库时 toolCallId 取表）。
fn live_item(entry: SubagentEntry, stored: Option<StoredSubagent>) -> (Item, State) {
    let state = match ended_status(entry.status) {
        None => State::Running {
            waiting: entry.waiting,
        },
        Some(status) => State::Ended(status),
    };
    let (stored_call, summary) = stored
        .map(|stored| (Some(stored.tool_use), stored.summary))
        .unwrap_or_default();
    let item = Item {
        child: entry.child.as_str().to_owned(),
        agent: Some(entry.agent.as_str().to_owned()),
        // 目录的 toolCallId 是派生它的 Agent 调用（Node 的 `part.callID`，§45.3），库里的就是它。表里的是这次运行属于的调用：
        // SendMessage 续跑之后是那次 SendMessage 调用（§61.1），所以先取库；只有启动的回执还没落库时才用表的（那时就是 Agent 调用）
        tool_call: stored_call
            .or(entry.tool_use)
            .map(|tool_use| tool_use.as_str().to_owned()),
        agent_type: entry.agent_type,
        title: title(Some(entry.description)),
        started_at: Some(entry.started_at_ms),
        ended_at: entry.ended_at_ms,
        summary,
    };
    (item, state)
}

/// 只在库里的一项（冷会话、本进程之前派生的）：都已结束，结局按调用的（Node `endedStatus` 的冷读）。
fn stored_item(stored: StoredSubagent) -> (Item, State) {
    let status = match &stored.call {
        StoredCall::Completed => QuerySessionSubagentsResultEndedItemsItemStatus::Success,
        StoredCall::Failed { error } if cancelled(error) => {
            QuerySessionSubagentsResultEndedItemsItemStatus::Cancelled
        }
        StoredCall::Failed { .. } => QuerySessionSubagentsResultEndedItemsItemStatus::Failed,
        StoredCall::Unfinished => QuerySessionSubagentsResultEndedItemsItemStatus::Lost,
    };
    let item = Item {
        child: stored.child.as_str().to_owned(),
        agent: stored.agent.map(|agent| agent.as_str().to_owned()),
        tool_call: Some(stored.tool_use.as_str().to_owned()),
        agent_type: stored
            .agent_type
            .unwrap_or_else(|| SUBAGENT_TYPE.to_owned()),
        title: title(stored.description),
        started_at: stored.started_at_ms,
        ended_at: stored.ended_at_ms,
        summary: stored.summary,
    };
    (item, State::Ended(status))
}

/// 失败的调用是不是停掉的（Node `CANCELLATION_PATTERN`，不分大小写）；子代理行的冷读也按它（§54.1）。
pub(super) fn cancelled(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    CANCELLATION_WORDS.iter().any(|word| error.contains(word))
}

/// `session.subagents` 的结果（§43.2、§45.3）：表里主线程派生的与库里的按子会话合并；在跑的按开始、结束的按结束的先后倒序，同值按子会话
/// id 倒序。
pub(crate) fn subagents(
    entries: Vec<SubagentEntry>,
    stored: Vec<StoredSubagent>,
    after: Option<&str>,
    limit: usize,
) -> Result<Value, Fault> {
    let mut stored: Vec<Option<StoredSubagent>> = stored.into_iter().map(Some).collect();
    let mut take = |child: &SessionId| {
        stored
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|stored| &stored.child == child))
            .and_then(Option::take)
    };
    let mut items: Vec<(Item, State)> = entries
        .into_iter()
        .filter(|entry| entry.owner.is_none())
        .map(|entry| {
            let known = take(&entry.child);
            live_item(entry, known)
        })
        .collect();
    items.extend(stored.into_iter().flatten().map(stored_item));
    let mut running = Vec::new();
    let mut ended = Vec::new();
    for (item, state) in items {
        match state {
            State::Running { waiting } => running.push((item, waiting)),
            State::Ended(status) => ended.push((item, status)),
        }
    }
    let newest = |left: &Item, right: &Item, at: fn(&Item) -> Option<u64>| {
        at(right)
            .cmp(&at(left))
            .then_with(|| right.child.cmp(&left.child))
    };
    running.sort_by(|(left, _), (right, _)| newest(left, right, |item| item.started_at));
    ended.sort_by(|(left, _), (right, _)| newest(left, right, |item| item.ended_at));
    let start = match after {
        None => 0,
        Some(cursor) => {
            let at = ended
                .iter()
                .position(|(item, _)| item.child == cursor)
                .ok_or_else(|| {
                    fault(
                        codes::PRECONDITION_INVALID_PAYLOAD,
                        format!(
                            "{ENDED_CURSOR}: {cursor} is not an ended subagent of this session"
                        ),
                    )
                })?;
            at + 1
        }
    };
    let page: Vec<_> = ended.iter().skip(start).take(limit).collect();
    let next_cursor = page
        .last()
        .filter(|_| start + page.len() < ended.len())
        .map(|(item, _)| item.child.clone());
    let result = QuerySessionSubagentsResult {
        child_session_ids: running
            .iter()
            .map(|(item, _)| item)
            .chain(ended.iter().map(|(item, _)| item))
            .map(|item| item.child.clone())
            .collect(),
        running: running.iter().map(running_item).collect(),
        ended: QuerySessionSubagentsResultEnded {
            total: ended.len() as u64,
            items: page.into_iter().map(ended_item).collect(),
            next_cursor,
        },
    };
    Ok(json!(result))
}

fn title(description: Option<String>) -> String {
    description
        .filter(|description| !description.is_empty())
        .unwrap_or_else(|| SUBAGENT_TITLE.to_owned())
}

fn running_item((item, waiting): &(Item, bool)) -> QuerySessionSubagentsResultRunningItem {
    QuerySessionSubagentsResultRunningItem {
        child_session_id: item.child.clone(),
        agent_id: item.agent.clone(),
        tool_call_id: item.tool_call.clone(),
        subagent_type: item.agent_type.clone(),
        title: item.title.clone(),
        // 在跑的没有摘要与结束（Node 相同）
        summary: None,
        started_at: item.started_at,
        ended_at: None,
        status: if *waiting {
            QuerySessionSubagentsResultRunningItemStatus::Waiting
        } else {
            QuerySessionSubagentsResultRunningItemStatus::Running
        },
    }
}

/// 结束了的子代理的结局；在跑的是 None。停放的子代理也是 Completed（它自己的事做完了）。
fn ended_status(status: SubagentStatus) -> Option<QuerySessionSubagentsResultEndedItemsItemStatus> {
    match status {
        SubagentStatus::Running => None,
        SubagentStatus::Completed => Some(QuerySessionSubagentsResultEndedItemsItemStatus::Success),
        SubagentStatus::Failed => Some(QuerySessionSubagentsResultEndedItemsItemStatus::Failed),
        SubagentStatus::Killed => Some(QuerySessionSubagentsResultEndedItemsItemStatus::Cancelled),
    }
}

fn ended_item(
    (item, status): &(Item, QuerySessionSubagentsResultEndedItemsItemStatus),
) -> QuerySessionSubagentsResultEndedItemsItem {
    QuerySessionSubagentsResultEndedItemsItem {
        child_session_id: item.child.clone(),
        agent_id: item.agent.clone(),
        tool_call_id: item.tool_call.clone(),
        subagent_type: item.agent_type.clone(),
        title: item.title.clone(),
        summary: item.summary.clone(),
        started_at: item.started_at,
        ended_at: item.ended_at,
        status: status.clone(),
    }
}

#[cfg(test)]
#[path = "reads_tests.rs"]
mod tests;
