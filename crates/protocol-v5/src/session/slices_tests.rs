//! 按片重建（protocol-v5.md §71.2）的对拍：一个会话（没有回合的历史、带工具与结果的长回合、思考块、输出上限之后的续写、停止取消的工具、
//! 出错的工具、比一页还长的回合）整份建投影、换骨架之后从尾往前翻；每一页里的骨架行按片重建，与整份建的那一页逐字段相同；片只读这一页的消息。

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, OUTPUT_LIMIT_RESUME_SOURCE, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::{InputEnd, SessionView, TurnOrigin};
use rust_cli_kernel_api::store::HistorySlice;
use serde_json::json;

use super::*;
use crate::session::projection::kit;
use crate::session::projection::residence::OUTLINE;

fn result(id: &str, assistant: &str, call: &str, text: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content: vec![ToolResultContent::Text {
                text: text.to_owned(),
            }],
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(assistant),
        },
    })
}

fn call(id: &str, call: &str) -> Message {
    kit::assistant(
        id,
        vec![ContentBlock::ToolUse {
            id: ToolUseId::new(call),
            name: rust_cli_kernel_api::tool_names::BASH.to_owned(),
            input: json!({"command": format!("echo {call}")}),
        }],
    )
}

/// 没有回合的历史（Node 写的）、长会话的回合，再加：思考 + 续写的回合、停止取消与出错的工具的回合、比一页还长的回合。
fn sample() -> SessionView {
    let mut messages = Vec::new();
    for n in 0..6 {
        messages.push(kit::user(&format!("msg_n{n}"), "old question"));
        messages.push(call(&format!("msg_na{n}"), &format!("toolu_n{n}")));
        messages.push(result(
            &format!("msg_nr{n}"),
            &format!("msg_na{n}"),
            &format!("toolu_n{n}"),
            "old",
            n == 2,
        ));
        messages.push(kit::assistant(
            &format!("msg_nt{n}"),
            vec![ContentBlock::Text {
                text: "old answer".to_owned(),
            }],
        ));
    }
    let long = kit::long_view(6, 40);
    let mut turns = long.turns.clone();
    messages.extend(long.messages.clone());
    // 思考 + 输出上限之后的续写（§19.9）：续写接进上一条的正文行
    messages.push(kit::user("msg_c0", "write a lot"));
    messages.push(kit::assistant(
        "msg_c1",
        vec![
            ContentBlock::Thinking {
                thinking: "plan".to_owned(),
                signature: Some("sig".to_owned()),
            },
            ContentBlock::Text {
                text: "first half".to_owned(),
            },
        ],
    ));
    messages.push(Message::User(UserMessage {
        id: MessageId::new("msg_c2"),
        content: vec![ContentBlock::Text {
            text: "continue".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: OUTPUT_LIMIT_RESUME_SOURCE.to_owned(),
        },
    }));
    messages.push(kit::assistant(
        "msg_c3",
        vec![ContentBlock::Text {
            text: " second half".to_owned(),
        }],
    ));
    let mut continued = kit::turn_record("turn_c", "msg_c0", TurnOrigin::Input);
    continued.ended = kit::ended(InputEnd::Completed, Vec::new());
    turns.push(continued);
    // 停止取消的工具（回合记录的 cancelled）与出错的工具
    messages.push(kit::user("msg_s0", "run two"));
    messages.push(call("msg_s1", "toolu_s1"));
    messages.push(result("msg_s2", "msg_s1", "toolu_s1", "boom", true));
    messages.push(call("msg_s3", "toolu_s3"));
    messages.push(result(
        "msg_s4",
        "msg_s3",
        "toolu_s3",
        "The user doesn't want to proceed",
        true,
    ));
    let mut stopped = kit::turn_record("turn_s", "msg_s0", TurnOrigin::Input);
    stopped.ended = kit::ended(InputEnd::Cancelled, vec![ToolUseId::new("toolu_s3")]);
    turns.push(stopped);
    // 比一页还长的回合
    messages.push(kit::user("msg_l0", "many"));
    for k in 0..12 {
        messages.push(call(&format!("msg_la{k}"), &format!("toolu_l{k}")));
        messages.push(result(
            &format!("msg_lr{k}"),
            &format!("msg_la{k}"),
            &format!("toolu_l{k}"),
            "ok",
            false,
        ));
    }
    let mut long_turn = kit::turn_record("turn_l", "msg_l0", TurnOrigin::Input);
    long_turn.ended = kit::ended(InputEnd::Completed, Vec::new());
    turns.push(long_turn);
    // 最后几个回合把上面的推出窗口
    let tail = kit::long_view(10, 10);
    for mut record in tail.turns {
        record.turn =
            rust_cli_kernel_api::ids::TurnId::new(format!("tail_{}", record.turn.as_str()));
        record.first = MessageId::new(format!("tail_{}", record.first.as_str()));
        turns.push(record);
    }
    // 最后几个回合的 id（消息与调用）都加前缀，不与前面的重
    let message = |id: &MessageId| MessageId::new(format!("tail_{}", id.as_str()));
    let tool = |id: &ToolUseId| ToolUseId::new(format!("tail_{}", id.as_str()));
    for item in tail.messages {
        messages.push(match item {
            Message::User(mut user) => {
                user.id = message(&user.id);
                if let UserOrigin::ToolResult { assistant } = &mut user.origin {
                    *assistant = message(assistant);
                }
                for block in &mut user.content {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                        *tool_use_id = tool(tool_use_id);
                    }
                }
                Message::User(user)
            }
            Message::Assistant(mut assistant) => {
                assistant.id = message(&assistant.id);
                for block in &mut assistant.content {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        *id = tool(id);
                    }
                }
                Message::Assistant(assistant)
            }
            other => other,
        });
    }
    let times = long
        .tool_times
        .into_iter()
        .chain(tail.tool_times.into_iter().map(|mut time| {
            time.tool_use = tool(&time.tool_use);
            time
        }));
    SessionView {
        tool_times: times.collect(),
        ..kit::view(messages, turns)
    }
}

/// 存储的契约（node-store.md §5.5）在一份视图上：`records` 里的存储消息按给出的顺序（同一条里的先后不变），开头在其中的回合，这些调用的起止。
fn history_of(view: &SessionView, records: &[MessageId]) -> HistorySlice {
    let messages: Vec<Message> = records
        .iter()
        .flat_map(|record| view.messages.iter().filter(move |m| m.record() == record))
        .cloned()
        .collect();
    let calls: Vec<&ToolUseId> = messages.iter().flat_map(Message::tool_uses).collect();
    HistorySlice {
        turns: view
            .turns
            .iter()
            .filter(|t| records.contains(&t.first))
            .cloned()
            .collect(),
        output_heads: Vec::new(),
        tool_times: view
            .tool_times
            .iter()
            .filter(|t| calls.contains(&&t.tool_use))
            .cloned()
            .collect(),
        messages,
    }
}

#[test]
fn every_page_rebuilt_from_its_slice_matches_the_whole_projection() {
    let view = sample();
    let (projection, content) = Projection::build(&view, kit::origin(false));
    let mut whole = kit::publisher();
    whole.adopt(content.clone());
    let mut thin = kit::publisher();
    thin.adopt(content);
    thin.thin(ROWS, OUTLINE, u64::MAX, |fields| {
        projection.resident(fields)
    });
    for limit in [3, 7, 40] {
        let (mut before, mut rebuilt) = (None::<String>, 0);
        loop {
            let page = thin
                .page(ROWS, CREATED_AT_SEQ, before.as_deref(), limit)
                .unwrap();
            let expected = whole
                .page(ROWS, CREATED_AT_SEQ, before.as_deref(), limit)
                .unwrap();
            let items = if page.outlined.is_empty() {
                page.items.clone()
            } else {
                rebuilt += 1;
                let slice = of(&projection, &thin, &page, view.meta.clone());
                // 片只读这一页的消息（有回合的整回合），远少于整段历史
                assert!(
                    slice.records.len() * 3 < view.messages.len(),
                    "{limit}: {:?}",
                    slice.records
                );
                let history = history_of(&view, &slice.records);
                rows_of(slice.base, history, page.clone()).unwrap()
            };
            assert_eq!(items, expected.items, "limit {limit} before {before:?}");
            if !page.more {
                break;
            }
            before = page.items[0]["id"].as_str().map(str::to_owned);
        }
        assert!(rebuilt > 1, "{limit}");
    }
}
