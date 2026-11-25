//! 订阅中的会话按窗口驻留（protocol-v5.md §67）：窗口外不再变、命令与查询不要整行的行只留骨架；快照不变；在跑回合的行回合结束之前留整行；
//! 种类与回合从骨架也读得到。

use std::collections::BTreeSet;

use rust_cli_kernel_api::ids::{MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::{KernelEvent, TurnOrigin, TurnOutcome};
use serde_json::{Value, json};

use super::super::kit::{ENDED, apply, assistant, live, long_view, origin, publisher, user};
use super::*;
use crate::consts::SESSION_SNAPSHOT_ROWS;
use crate::publisher::{ID_KEY, Publisher};

/// 长会话的回合数：每回合 8 行（回合头、输入、5 个调用、回答），远多于窗口。
const TURNS: usize = 20;
const OUTPUT_BYTES: usize = 100;
/// 在跑的那一轮的调用数：多于窗口，最早的几个调用在回合结束之前就被挤出窗口。
const CALLS: usize = SESSION_SNAPSHOT_ROWS + 10;

/// 全部行的 id，按 `(createdAtSeq, id)` 的顺序。
fn ids(publisher: &Publisher) -> Vec<String> {
    publisher
        .page(ROWS, CREATED_AT_SEQ, None, usize::MAX)
        .map(|page| page.items)
        .unwrap_or_default()
        .iter()
        .map(|item| item[ID_KEY].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn whole(publisher: &Publisher, id: &str) -> bool {
    publisher.entity(ROWS, id).is_some()
}

/// 没有订阅的出站队列：换骨架不受「订阅都已收到」的限制（§68.0 第 5 条）。
fn no_subscribers() -> crate::outbox::Outbox {
    crate::outbox::Outbox::new(1, usize::MAX)
}

#[test]
fn a_built_session_keeps_whole_rows_only_in_the_window_and_for_pinned_rows() {
    let view = long_view(TURNS, OUTPUT_BYTES);
    let (projection, content) = Projection::build(&view, origin(false));
    let mut thinned = publisher();
    thinned.adopt(content.clone());
    thin(&mut thinned, &projection, &no_subscribers());
    let mut reference = publisher();
    reference.adopt(content);
    let order = ids(&thinned);
    assert_eq!(order, ids(&reference));
    let (outside, window) = order.split_at(order.len() - SESSION_SNAPSHOT_ROWS);
    assert!(window.iter().all(|id| whole(&thinned, id)));
    for id in outside {
        let kind = thinned.field(ROWS, id, keys::KIND).and_then(Value::as_str);
        let pinned = matches!(kind, Some(kinds::TURN_HEADER | kinds::USER_INPUT));
        assert_eq!(whole(&thinned, id), pinned, "{id} ({kind:?})");
        // 骨架里排序、种类、回合与版本都在
        assert_eq!(
            thinned.field(ROWS, id, keys::TURN_ID),
            reference
                .entity(ROWS, id)
                .and_then(|row| row.get(keys::TURN_ID))
        );
        assert_eq!(thinned.version(ROWS, id), reference.version(ROWS, id));
    }
    // 快照只带窗口：与全部整行的那一份逐字段相同
    let snapshot = |publisher: &Publisher| {
        serde_json::to_value(publisher.frame_for(None, "s", false)).unwrap()["body"].clone()
    };
    assert_eq!(snapshot(&thinned), snapshot(&reference));
    // 一页里标出骨架：最早的调用行只有骨架的键
    let page = thinned
        .page(ROWS, CREATED_AT_SEQ, None, usize::MAX)
        .unwrap();
    assert!(page.outlined.contains(&"toolu_0_0".to_owned()));
    let first_call = page
        .items
        .iter()
        .find(|item| item[ID_KEY] == "toolu_0_0")
        .unwrap();
    let keys_of: BTreeSet<&str> = first_call
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys_of,
        BTreeSet::from([ID_KEY, "v", CREATED_AT_SEQ, keys::KIND, keys::TURN_ID])
    );
    // 同样的内容再 adopt：只有 state，不发新快照（骨架按骨架的键比）
    let (_, again) = Projection::build(&view, origin(false));
    assert!(!thinned.adopt(again));
}

/// 回合在跑：这一轮的行（挤出窗口的也算）留整行；回合结束之后窗口外的换成骨架，回合头与输入照旧整行。
#[test]
fn rows_of_the_running_turn_stay_whole_until_it_finishes() {
    let (mut projection, mut publisher) = live();
    let mut events = vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new("turn_a"),
            command: None,
            origin: TurnOrigin::Input,
            started_at: 100,
            works: Vec::new(),
        },
        KernelEvent::MessageDone {
            message: user("msg_ua", "go"),
        },
    ];
    for k in 0..CALLS {
        let (call, asked) = (format!("toolu_a_{k}"), format!("msg_aa{k}"));
        events.push(KernelEvent::MessageDone {
            message: assistant(
                &asked,
                vec![ContentBlock::ToolUse {
                    id: ToolUseId::new(&call),
                    name: rust_cli_kernel_api::tool_names::READ.to_owned(),
                    input: json!({"file_path": "/Users/dev/a/f.txt"}),
                }],
            ),
        });
        events.push(KernelEvent::MessageDone {
            message: Message::User(UserMessage {
                id: MessageId::new(format!("msg_ra{k}")),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new(&call),
                    content: vec![ToolResultContent::Text {
                        text: "ok".to_owned(),
                    }],
                    is_error: false,
                }],
                origin: UserOrigin::ToolResult {
                    assistant: MessageId::new(&asked),
                },
            }),
        });
    }
    apply(&mut publisher, &mut projection, events);
    thin(&mut publisher, &projection, &no_subscribers());
    assert!(ids(&publisher).iter().all(|id| whole(&publisher, id)));
    apply(
        &mut publisher,
        &mut projection,
        vec![KernelEvent::TurnFinished {
            turn: TurnId::new("turn_a"),
            outcome: TurnOutcome::Success,
            usage: None,
            time: ENDED,
            files: Vec::new(),
        }],
    );
    thin(&mut publisher, &projection, &no_subscribers());
    assert!(!whole(&publisher, "toolu_a_0"));
    assert!(whole(&publisher, "turn_a"));
    assert!(whole(&publisher, "msg_ua"));
    assert!(whole(&publisher, &format!("toolu_a_{}", CALLS - 1)));
    // 种类与回合从骨架读（`turn.retry`、`turn.fork` 判目标行）
    assert_eq!(
        crate::session::rewind::row_kind(Some(&publisher), "toolu_a_0"),
        Some((kinds::TOOL_CALL, "turn_a"))
    );
}

/// hook 行留整行（protocol-v5.md §71.0 第 4 条）：它的事实只在会话 actor 的内存里（视图的 `hooks`），库里没有，换成骨架之后按片重建不出。
#[test]
fn hook_rows_stay_whole() {
    let (projection, _) = kit::live();
    let row = rows::base(hook_rows::KIND, "turn_1", 0, true, None);
    assert!(projection.resident(&row));
}
