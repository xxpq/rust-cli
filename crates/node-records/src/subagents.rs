//! 库里的子代理（docs/specs/rust-cli/session-lifecycle.md §10.46）：父会话的 Agent 调用怎么关联到子会话、子会话最后的回答，
//! 照 Node 子代理目录的冷读（`subagent-session-query.ts` 的 `candidateFromToolPart` 与 `lastChildOutcome`）。记录的格式是纯函数；
//! [`stored`] 经 [`Rows`] 读父会话与子会话（`StoreQueries::subagents`，本机与云端共用）。

use std::borrow::Cow;
use std::collections::HashMap;

use rust_cli_kernel_api::agents::{StoredCall, StoredSubagent};
use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_kernel_api::tool_names;
use serde_json::{Map, Value};

use crate::branch::{Branch, select_branch};
use crate::codecs::decode_part;
use crate::facts::live_session;
use crate::raw::{RawPart, RawRecord};
use crate::row::WithParts;
use crate::rows::Rows;

/// 派生子代理的工具名（Node `SUBAGENT_TOOL_NAMES`：本工程与 Node 现在的 Agent，Node 早期的 `Task`、`subagent`）。
const SUBAGENT_TOOLS: [&str; 3] = [tool_names::AGENT, "Task", "subagent"];
/// 子代理的子会话 id 的前缀：`sess_subagent_<agentId>`（Node `createSessionId("subagent_" + agentId)`）。
const CHILD_SESSION_PREFIX: &str = "sess_subagent_";
/// 后台启动的回执里写 agentId 的那一行（Node `agentIdFromLaunchAcknowledgement`）。
const AGENT_ID_LINE: &str = "agentId:";

/// 一个 Agent 调用的 tool part 关联到的子会话（还没有读子会话）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentCall {
    pub tool_use: ToolUseId,
    pub child: String,
    pub agent: Option<String>,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub call: StoredCall,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    /// 调用失败时的错误（Node 的 summary 先取它）。
    pub error: Option<String>,
}

/// 子会话最后一条 assistant 给目录的：文字（没有时是错误）与完成的时刻。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildOutcome {
    pub summary: Option<String>,
    pub completed_at_ms: Option<u64>,
}

/// 去掉首尾空白之后不为空的字符串（Node `stringField` / `nonEmptyString`）。
fn text(value: &Value) -> Option<String> {
    let trimmed = value.as_str()?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 第一个有值的键。
fn first(record: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| record.get(*key).and_then(text))
}

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// 结果文字里的 `agentId: <id>` 一行（Node 写的后台回执与前台的交回都带它）。
fn agent_id_line(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let rest = line.strip_prefix(AGENT_ID_LINE)?.trim_start();
        let id: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '(')
            .collect();
        (!id.is_empty()).then_some(id)
    })
}

/// 一个 tool part 是不是派生子代理的调用、关联到哪个子会话（Node `candidateFromToolPart` 的冷读：没有内存里的事件）。
/// 结果是 JSON 的（Node 早期的写法）先取它；再取结果文字里的 `agentId:` 行；再取 `metadata`（本工程写的，node-store.md §4.6）。
pub fn call(part: &Value) -> Option<SubagentCall> {
    if part["type"] != "tool" || !SUBAGENT_TOOLS.contains(&part["tool"].as_str()?) {
        return None;
    }
    let state = &part["state"];
    let status = state["status"].as_str();
    let completed = (status == Some("completed"))
        .then(|| state["output"].as_str())
        .flatten();
    let output = completed
        .and_then(|output| serde_json::from_str::<Value>(output).ok())
        .map(|value| object(&value))
        .unwrap_or_default();
    let input = object(&state["input"]);
    let mut metadata = object(&part["metadata"]);
    metadata.extend(object(&state["metadata"]));
    let agent = first(&output, &["agentId"])
        .or_else(|| completed.and_then(agent_id_line))
        .or_else(|| first(&metadata, &["agentId"]));
    let child = first(&output, &["childSessionId"])
        .or_else(|| first(&metadata, &["childSessionId"]))
        .or_else(|| {
            agent
                .as_ref()
                .map(|id| format!("{CHILD_SESSION_PREFIX}{id}"))
        })?;
    let error = (status == Some("error"))
        .then(|| text(&state["error"]))
        .flatten();
    let call = match status {
        Some("completed") => StoredCall::Completed,
        Some("error") => StoredCall::Failed {
            error: error.clone().unwrap_or_default(),
        },
        _ => StoredCall::Unfinished,
    };
    Some(SubagentCall {
        tool_use: ToolUseId::new(part["callID"].as_str()?),
        child,
        agent,
        agent_type: first(&output, &["agentType"])
            .or_else(|| first(&metadata, &["agentType"]))
            .or_else(|| first(&input, &["subagent_type", "agent", "agentType"])),
        description: first(&output, &["description"])
            .or_else(|| first(&input, &["description"]))
            .or_else(|| first(&metadata, &["description"]))
            .or_else(|| first(&input, &["prompt"])),
        call,
        started_at_ms: state["time"]["start"].as_u64(),
        ended_at_ms: state["time"]["end"].as_u64(),
        error,
    })
}

/// 父会话活动分支上的调用，按子会话去重：后写的覆盖，位置取第一次出现的（Node 的 `Map`）。
pub fn calls(messages: &[std::borrow::Cow<'_, WithParts>]) -> Vec<SubagentCall> {
    let mut found: Vec<SubagentCall> = Vec::new();
    for message in messages {
        add_calls(&mut found, message);
    }
    found
}

/// 一条消息里的调用并进已经找到的（[`calls`] 的去重规则）。
pub(crate) fn add_calls(found: &mut Vec<SubagentCall>, message: &WithParts) {
    for call in message.parts.iter().filter_map(call) {
        match found.iter_mut().find(|known| known.child == call.child) {
            Some(known) => *known = call,
            None => found.push(call),
        }
    }
}

/// 子会话最后一条 assistant（Node `lastChildOutcome`）：没有 `ignored` 的文字，去掉首尾空白，用空行连接；没有文字时取它的错误
/// （`error.data` 的 message / error / detail，再取 `error.name`）。
pub fn outcome(messages: &[WithParts]) -> ChildOutcome {
    let Some(last) = messages
        .iter()
        .rev()
        .find(|message| message.info["role"] == "assistant")
    else {
        return ChildOutcome::default();
    };
    let words: Vec<String> = last
        .parts
        .iter()
        .filter(|part| part["type"] == "text" && part["ignored"] != Value::Bool(true))
        .filter_map(|part| text(&part["text"]))
        .collect();
    let error = &last.info["error"];
    let summary = if words.is_empty() {
        first(&object(&error["data"]), &["message", "error", "detail"])
            .or_else(|| text(&error["name"]))
    } else {
        Some(words.join("\n\n"))
    };
    ChildOutcome {
        summary,
        completed_at_ms: last.info["time"]["completed"].as_u64(),
    }
}

/// 子代理的子会话的种类（Node `taskType`）：别的子会话（工作流等）不进子代理目录。
const SUBAGENT_CHILD: &str = "subagent_child";

/// 这个会话派生过的子代理：父会话活动分支上的 Agent 调用与它们的子会话。只读。
pub fn stored(conn: &dyn Rows, parent: &SessionId) -> Result<Vec<StoredSubagent>, StoreError> {
    let calls = branch_calls(conn, parent)?;
    resolve(conn, calls)
}

/// 父会话活动分支上的 Agent 调用，按子会话去重（[`calls`] 的规则）。会话不在或已删除时 `NotFound`。
pub fn branch_calls(conn: &dyn Rows, parent: &SessionId) -> Result<Vec<SubagentCall>, StoreError> {
    let row = live_session(conn, parent)?;
    // 修性能缺陷（PERF-SWITCH，node-store.md §5.4）：以前读出父会话的全部 part 行、解码活动分支上的每一个 part，只为找 Agent 调用；v5 一次冷切换
    // 读两遍，占 CPU 的近一半。分支只按消息 id 选（同以前），part 只取 Agent 的 tool part（库里按 JSON 成员筛），去重与取法不变
    let messages: Vec<WithParts> = conn
        .message_ids(parent.as_str())?
        .into_iter()
        .map(|id| WithParts {
            info: serde_json::json!({ "id": id }),
            parts: Vec::new(),
        })
        .collect();
    // Node `activeBranchMessages`：只按回退游标选分支，压缩点之前的调用也算
    let branch = Branch::from_revert(row.revert.as_ref());
    let active = select_branch(messages.iter().map(Cow::Borrowed).collect(), &branch);
    let mut parts: HashMap<String, Vec<RawPart>> = HashMap::new();
    for (message, part) in conn.tool_parts(parent.as_str(), &SUBAGENT_TOOLS)? {
        parts.entry(message).or_default().push(part);
    }
    let mut calls = Vec::new();
    for message in &active {
        // 与整份解码同一个函数（`RawRecord::decode` 逐个 part 调它）；一个 id 在分支里出现两次时同以前一样解码两次
        let decoded = parts
            .get(message.id())
            .into_iter()
            .flatten()
            .map(|part| decode_part(&part.data, &part.id, parent.as_str(), message.id()))
            .collect::<serde_json::Result<Vec<Value>>>()
            .map_err(crate::storage)?;
        add_calls(
            &mut calls,
            &WithParts {
                info: Value::Null,
                parts: decoded,
            },
        );
    }
    Ok(calls)
}

/// 调用关联到的子会话（不在、或不是子代理的子会话的去掉）与子会话最后一条回答。
pub fn resolve(
    conn: &dyn Rows,
    calls: Vec<SubagentCall>,
) -> Result<Vec<StoredSubagent>, StoreError> {
    let mut found = Vec::new();
    for call in calls {
        let Some(child) = conn
            .session(&call.child)?
            .filter(|child| child.task_type == SUBAGENT_CHILD)
        else {
            continue;
        };
        // 只要子会话最后一条 assistant（`outcome`）：只解码它
        let last = conn
            .messages_raw(&call.child)?
            .iter()
            .rev()
            .find(|record| record.info["role"] == "assistant")
            .map(RawRecord::decode)
            .transpose()
            .map_err(crate::storage)?;
        let outcome = outcome(last.as_slice());
        found.push(StoredSubagent {
            tool_use: call.tool_use,
            child: SessionId::new(call.child),
            agent: call.agent.map(AgentId::new),
            agent_type: call.agent_type,
            description: call.description,
            call: call.call,
            started_at_ms: call.started_at_ms,
            // Node 的冷读：调用结束 → 子会话最后一条回答完成 → 子会话最后更新
            ended_at_ms: call
                .ended_at_ms
                .or(outcome.completed_at_ms)
                .or(u64::try_from(child.time_updated).ok()),
            // Node：失败的调用先取它的错误，再取子会话最后的回答
            summary: call.error.or(outcome.summary),
        });
    }
    Ok(found)
}

#[cfg(test)]
#[path = "subagents_tests.rs"]
mod tests;
