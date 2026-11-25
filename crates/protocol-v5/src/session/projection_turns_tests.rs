//! 没有回合记录的历史按用户输入分回合（protocol-v5.md §75.3）：每条开回合的输入一个回合（回合头 + 这一段的行），第一条输入之前的不属于回合；
//! 记录的回合里、不晚于它结束的输入留在里面，结束之后别的写者续写的输入另起回合；有记录的会话不出现合成的回合。

use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::session_api::{InputEnd, SessionView, TurnOrigin};
use rust_cli_kernel_api::store::InputSource;
use rust_cli_kernel_api::turn_walk::UNRECORDED_TURN_PREFIX;
use serde_json::Value;

use super::kit::{ENDED, assistant, ended, long_view, origin, turn_record, user, view};
use super::*;

/// 两边写的消息 id 带创建的毫秒（`msg_<36 进制毫秒>_<名字>`）。
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

fn text(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: text.to_owned(),
    }]
}

/// 建出的行按号排好：(id, kind, turnId)。
fn rows_of(view: &SessionView) -> Vec<(String, String, String)> {
    let (_, content) = Projection::build(view, origin(false));
    let mut rows: Vec<(u64, String, String, String)> = content.collections[ROWS]
        .iter()
        .map(|(id, fields)| {
            let text = |key: &str| fields[key].as_str().unwrap_or_default().to_owned();
            (
                fields[CREATED_AT_SEQ].as_u64().unwrap(),
                id.clone(),
                text(rows::keys::KIND),
                text(rows::keys::TURN_ID),
            )
        })
        .collect();
    rows.sort();
    rows.into_iter()
        .map(|(_, id, kind, turn)| (id, kind, turn))
        .collect()
}

fn row(id: &str, kind: &str, turn: &str) -> (String, String, String) {
    (id.to_owned(), kind.to_owned(), turn.to_owned())
}

#[test]
fn history_without_records_gets_a_turn_per_input() {
    let (u1, u2) = (at(1_000, "u1"), at(2_000, "u2"));
    let (t1, t2) = (
        format!("{UNRECORDED_TURN_PREFIX}{u1}"),
        format!("{UNRECORDED_TURN_PREFIX}{u2}"),
    );
    let mut node = view(
        vec![
            assistant("msg_a0", text("before any input")),
            user(&u1, "one"),
            assistant("msg_a1", text("first")),
            user(&u2, "two"),
            assistant("msg_a2", text("second")),
        ],
        Vec::new(),
    );
    node.sources = vec![InputSource {
        message: MessageId::new(&u1),
        command: CommandId::new("cmd_u1"),
    }];
    assert_eq!(
        rows_of(&node),
        [
            row("msg_a0", "assistantText", ""),
            row(&t1, "turnHeader", &t1),
            row(&u1, "userInput", &t1),
            row("msg_a1", "assistantText", &t1),
            row(&t2, "turnHeader", &t2),
            row(&u2, "userInput", &t2),
            row("msg_a2", "assistantText", &t2),
        ]
    );
    // 回合头：来源是用户输入、开始的时刻取自输入的消息 id、命令是这条输入的、读成完成，没有结束的时刻与耗时
    let (_, content) = Projection::build(&node, origin(false));
    let header = &content.collections[ROWS][&t1];
    assert_eq!(header[rows::keys::ORIGIN], rows::origin(TurnOrigin::Input));
    assert_eq!(header[rows::keys::STATE], "completed");
    assert_eq!(header[rows::keys::STARTED_AT], 1_000.0);
    assert_eq!(header["sourceCommandId"], "cmd_u1");
    assert_eq!(header.get(rows::keys::ENDED_AT), None);
    assert_eq!(
        content.collections[ROWS][&t2].get("sourceCommandId"),
        None::<&Value>
    );
}

#[test]
fn an_input_after_a_recorded_turn_ended_opens_a_turn() {
    // 本工程的回合（结束于 ENDED）：成批进来的后续输入建于回合里，留在回合里；结束之后 Node 续写的输入另起回合
    let (u1, batched, node) = (
        at(ENDED.ended_at - 2_000, "u1"),
        at(ENDED.ended_at - 1_000, "u1b"),
        at(ENDED.ended_at + 1_000, "u2"),
    );
    let mut record = turn_record("turn_1", &u1, TurnOrigin::Input);
    record.ended = ended(InputEnd::Completed, Vec::new());
    let mixed = view(
        vec![
            user(&u1, "one"),
            assistant("msg_a1", text("first")),
            user(&batched, "queued"),
            assistant("msg_a1b", text("more")),
            user(&node, "from node"),
            assistant("msg_a2", text("node answer")),
        ],
        vec![record],
    );
    let synthetic = format!("{UNRECORDED_TURN_PREFIX}{node}");
    let turns: Vec<(String, String)> = rows_of(&mixed)
        .into_iter()
        .map(|(id, _, turn)| (id, turn))
        .collect();
    assert_eq!(
        turns,
        [
            ("turn_1".to_owned(), "turn_1".to_owned()),
            (u1, "turn_1".to_owned()),
            ("msg_a1".to_owned(), "turn_1".to_owned()),
            (batched, "turn_1".to_owned()),
            ("msg_a1b".to_owned(), "turn_1".to_owned()),
            (synthetic.clone(), synthetic.clone()),
            (node, synthetic.clone()),
            ("msg_a2".to_owned(), synthetic),
        ]
    );
}

/// 有记录的会话（本工程写的长会话：每个回合一条记录，回合里有工具）：一个合成的回合也没有。
#[test]
fn a_recorded_session_has_no_synthetic_turns() {
    let rows = rows_of(&long_view(20, 64));
    assert!(!rows.is_empty());
    assert!(
        rows.iter()
            .all(|(_, _, turn)| !turn.starts_with(UNRECORDED_TURN_PREFIX)),
        "{rows:?}"
    );
}
