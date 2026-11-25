//! 只建了后缀的投影的行号（protocol-v5.md §73.1）：从 2^32 起编，活会话的新行接着往后编；激活从原来的下沿与号起建，与冷的那份逐项相同；
//! 改写历史整段建、从 0 起；往前补的一段接在最早一行之前、号连着。

use rust_cli_kernel_api::session_api::{KernelEvent, SessionView};
use serde_json::Value;

use super::kit::{self, apply, long_view, user};
use super::*;
use crate::publisher::Publisher;

/// 长会话的最后 `turns` 个回合（只有它们的消息与回合记录，同冷读的段）。
fn tail_of(whole: &SessionView, turns: usize) -> SessionView {
    let keep: Vec<String> = whole.turns[whole.turns.len() - turns..]
        .iter()
        .map(|record| record.turn.as_str().to_owned())
        .collect();
    let first = whole
        .messages
        .iter()
        .position(|message| message.id().as_str() == format!("msg_u{}", whole.turns.len() - turns))
        .unwrap();
    SessionView {
        messages: whole.messages[first..].to_vec(),
        turns: whole
            .turns
            .iter()
            .filter(|record| keep.contains(&record.turn.as_str().to_owned()))
            .cloned()
            .collect(),
        ..whole.clone()
    }
}

fn numbers(content: &Content) -> Vec<(u64, String)> {
    let mut numbers: Vec<(u64, String)> = content.collections[ROWS]
        .iter()
        .map(|(id, fields)| (fields[CREATED_AT_SEQ].as_u64().unwrap(), id.clone()))
        .collect();
    numbers.sort();
    numbers
}

fn tail_origin(start: Start) -> Origin {
    Origin {
        start,
        ..kit::origin(false)
    }
}

/// 冷读的段从 2^32 起编、`rowsWindow.earlier` 为真；激活时由整段的活视图从原来的下沿与号建，行与号逐项相同。
#[test]
fn a_tail_numbers_from_the_anchor_and_activation_keeps_the_numbers() {
    let whole = long_view(6, 10);
    let tail = tail_of(&whole, 2);
    let (cold, content) = Projection::build(&tail, tail_origin(Start::Tail));
    let numbers_cold = numbers(&content);
    assert_eq!(numbers_cold[0].0, crate::consts::ROW_NUMBER_ANCHOR);
    assert!(cold.earlier());
    assert_eq!(content.state["rowsWindow"]["earlier"], Value::Bool(true));
    let (live, live_content) = Projection::build(
        &whole,
        Origin {
            start: cold.activation(),
            ..kit::origin(true)
        },
    );
    assert_eq!(numbers(&live_content), numbers_cold);
    assert_eq!(live_content.collections[ROWS], content.collections[ROWS]);
    assert!(live.earlier());
    assert_eq!(live.floor(), cold.floor());
}

/// 活会话的新行接着最大的号往后编（在 2^32 之上），全序与显示顺序一致。
#[test]
fn live_rows_after_a_tail_number_above_it() {
    let whole = long_view(4, 10);
    let tail = tail_of(&whole, 1);
    let (mut projection, content) = Projection::build(&tail, tail_origin(Start::Tail));
    let max = numbers(&content).last().unwrap().0;
    let mut publisher: Publisher = kit::publisher();
    publisher.adopt(content);
    apply(
        &mut publisher,
        &mut projection,
        vec![KernelEvent::MessageDone {
            message: user("msg_new", "later"),
        }],
    );
    let added = publisher.entity(ROWS, "msg_new").unwrap();
    assert_eq!(added[CREATED_AT_SEQ].as_u64(), Some(max + 1));
}

/// 改写历史之后整段建：号从 0 起、`earlier` 为假（新快照，号跨 epoch 不可比，§73.1）。
#[test]
fn a_rewrite_rebuilds_the_whole_history_from_zero() {
    let whole = long_view(4, 10);
    let (tail, _) = Projection::build(&tail_of(&whole, 1), tail_origin(Start::Tail));
    let (rebuilt, content) = Projection::build(&whole, tail.origin());
    assert_eq!(numbers(&content)[0].0, 0);
    assert!(!rebuilt.earlier());
    assert_eq!(content.state["rowsWindow"]["earlier"], Value::Bool(false));
}

/// 激活时原来的下沿在活视图里找不到（别处改写了历史）：整段建、号从 0 起（内容不同，Publisher 发新快照）。
#[test]
fn activation_without_the_floor_builds_everything() {
    let whole = long_view(3, 10);
    let start = Start::Floor {
        record: rust_cli_kernel_api::ids::MessageId::new("msg_gone"),
        number: crate::consts::ROW_NUMBER_ANCHOR,
    };
    let (projection, content) = Projection::build(
        &whole,
        Origin {
            start,
            ..kit::origin(true)
        },
    );
    assert_eq!(numbers(&content)[0].0, 0);
    assert!(!projection.earlier());
}
