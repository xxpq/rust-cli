//! 按显示顺序把消息归到回合（node-store.md §5.6）：没有记录的历史按开回合的输入切；记录的回合里、不晚于它结束的输入留在里面（自己的输入、成批进来的后续输入）；
//! 回合结束之后别的写者续写的输入另起回合。

use super::*;
use crate::message::{AssistantMessage, ContentBlock, UserMessage, UserOrigin};
use crate::session_api::TurnOrigin;

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin: UserOrigin::Human,
    })
}

fn meta(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "notice".into(),
        }],
        origin: UserOrigin::Meta {
            source: "task_notification".into(),
        },
    })
}

fn assistant(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "answer".into(),
        }],
        model: "m".into(),
        stop_reason: None,
        usage: None,
    })
}

fn record(turn: &str, first: &str, origin: TurnOrigin) -> TurnRecord {
    TurnRecord::opened(TurnId::new(turn), MessageId::new(first), origin, 1)
}

/// 每条消息走完之后的回合（空串 = 不属于回合）。
fn turns(records: &[TurnRecord], messages: &[Message]) -> Vec<String> {
    let mut walk = TurnWalk::new(records);
    let mut current = String::new();
    messages
        .iter()
        .map(|message| {
            match walk.step(message) {
                Some(Opened::Recorded(record)) => current = record.turn.as_str().to_owned(),
                Some(Opened::Unrecorded(turn)) => current = turn.as_str().to_owned(),
                None => {}
            }
            current.clone()
        })
        .collect()
}

#[test]
fn history_without_records_is_cut_at_each_input() {
    let messages = [
        assistant("a0"),
        user("u1", "one"),
        assistant("a1"),
        meta("n1"),
        assistant("a1b"),
        user("u2", "two"),
        user("i2", INTERRUPT),
        assistant("a2"),
        user(
            "b2",
            "<bash-stdout>ok</bash-stdout><bash-stderr></bash-stderr>",
        ),
    ];
    assert_eq!(
        turns(&[], &messages),
        [
            "",
            "turn_of_u1",
            "turn_of_u1",
            "turn_of_u1",
            "turn_of_u1",
            "turn_of_u2",
            "turn_of_u2",
            "turn_of_u2",
            "turn_of_u2",
        ]
    );
}

const INTERRUPT: &str = "[Request interrupted by user]";

/// 消息 id 带创建的毫秒（两边写的格式 `msg_<36 进制毫秒>_<uuid>`）。
fn at(ms: u64, name: &str) -> String {
    let mut digits = String::new();
    let mut left = ms;
    loop {
        digits.insert(
            0,
            char::from_digit(u32::try_from(left % 36).unwrap(), 36).unwrap(),
        );
        left /= 36;
        if left == 0 {
            break;
        }
    }
    format!("msg_{digits}_{name}")
}

fn ended_at(mut record: TurnRecord, ms: u64) -> TurnRecord {
    record.ended = Some(crate::store::TurnEnded {
        outcome: crate::session_api::InputEnd::Completed,
        cancelled: Vec::new(),
        time: Some(crate::session_api::TurnTime {
            ended_at: ms,
            active_ms: 0,
        }),
        files: Vec::new(),
    });
    record
}

#[test]
fn inputs_inside_a_recorded_turn_stay_in_it() {
    // 第一条是输入本身；成批进回合的后续输入（建于回合里）留在回合里；没有结束时刻的记录同样
    let (u1, u2, u3) = (at(1000, "u1"), at(1500, "u2"), at(9000, "u3"));
    let records = [ended_at(record("turn_1", &u1, TurnOrigin::Input), 2000)];
    let messages = [
        user(&u1, "one"),
        assistant("a1"),
        user(&u2, "batched"),
        assistant("a2"),
    ];
    assert_eq!(
        turns(&records, &messages),
        ["turn_1", "turn_1", "turn_1", "turn_1"]
    );
    let open = [record("turn_1", &u1, TurnOrigin::Input)];
    let messages = [user(&u1, "one"), assistant("a1"), user(&u3, "later")];
    assert_eq!(turns(&open, &messages), ["turn_1", "turn_1", "turn_1"]);
    // 第一条在输入之前
    let records = [record("turn_1", "x1", TurnOrigin::Input)];
    let messages = [meta("x1"), user(&u1, "one"), assistant("a1")];
    assert_eq!(turns(&records, &messages), ["turn_1", "turn_1", "turn_1"]);
}

#[test]
fn an_input_after_a_recorded_turn_ended_opens_a_turn() {
    // 回合结束之后的输入是别的写者续写的（Node）：另起回合；之后的记录照常
    let (n1, u2, u3) = (at(1000, "n1"), at(3000, "u2"), at(5000, "u3"));
    let records = [
        ended_at(record("turn_n", &n1, TurnOrigin::Notice), 2000),
        record("turn_3", &u3, TurnOrigin::Input),
    ];
    let u2_turn = format!("turn_of_{u2}");
    let messages = [
        meta(&n1),
        assistant("a1"),
        user(&u2, "two"),
        assistant("a2"),
        user(&u3, "three"),
    ];
    assert_eq!(
        turns(&records, &messages),
        [
            "turn_n",
            "turn_n",
            u2_turn.as_str(),
            u2_turn.as_str(),
            "turn_3"
        ]
    );
}
