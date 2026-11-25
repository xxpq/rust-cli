//! `MemoryStore` 的库里的子代理（node-store `subagents` 的内存替身，session-lifecycle.md §10.46）：会话里的 `SubagentLinked`，说明取
//! 调用的 input，summary 取子会话最后一条 assistant 的文字。内存里没有 tool part 的时刻与结局：调用都算完成、时刻为空。

use std::collections::HashMap;

use crate::agents::{StoredCall, StoredSubagent};
use crate::ids::{SessionId, ToolUseId};
use crate::message::{ContentBlock, Message};
use crate::store::{Fact, SessionMeta, StoreError};

type Sessions = HashMap<SessionId, (SessionMeta, Vec<Fact>)>;

pub(super) fn subagents(
    sessions: &Sessions,
    session: &SessionId,
) -> Result<Vec<StoredSubagent>, StoreError> {
    let (_, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
        session: session.clone(),
    })?;
    let mut found: Vec<StoredSubagent> = Vec::new();
    for fact in facts {
        let Fact::SubagentLinked {
            tool_use,
            agent,
            child,
            agent_type,
        } = fact
        else {
            continue;
        };
        let stored = StoredSubagent {
            tool_use: tool_use.clone(),
            child: child.clone(),
            agent: Some(agent.clone()),
            agent_type: Some(agent_type.clone()),
            description: description(facts, tool_use),
            call: StoredCall::Completed,
            started_at_ms: None,
            ended_at_ms: None,
            summary: answer(sessions, child),
        };
        // 同一个子会话后写的覆盖，位置取第一次（同 node-store）
        match found.iter_mut().find(|known| &known.child == child) {
            Some(known) => *known = stored,
            None => found.push(stored),
        }
    }
    Ok(found)
}

fn description(facts: &[Fact], tool_use: &ToolUseId) -> Option<String> {
    facts.iter().find_map(|fact| match fact {
        Fact::MessageAppended {
            message: Message::Assistant(message),
        } => message.content.iter().find_map(|block| match block {
            ContentBlock::ToolUse { id, input, .. } if id == tool_use => {
                input["description"].as_str().map(str::to_owned)
            }
            _ => None,
        }),
        _ => None,
    })
}

/// 子会话最后一条 assistant 的文字，去掉首尾空白，用空行连接。
fn answer(sessions: &Sessions, child: &SessionId) -> Option<String> {
    let (_, facts) = sessions.get(child)?;
    facts.iter().rev().find_map(|fact| match fact {
        Fact::MessageAppended {
            message: Message::Assistant(message),
        } => Some(
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.trim()),
                    _ => None,
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        _ => None,
    })
}
