//! 行 → 存储消息的索引（protocol-v5.md §71.2）：有回合的行取整回合，没有回合的行取行号所在的那一条；工具结果归它的 assistant。

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};

use super::Walk;

fn user(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: Vec::new(),
        origin: UserOrigin::Human,
    })
}

fn result(id: &str, assistant: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("toolu_1"),
            content: Vec::new(),
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(assistant),
        },
    })
}

/// 走过：没有回合的 n1（行 0、1）、n2（行 2）；回合 t1：u1（回合头 3、输入 4）、a1（5、6）、a1 的结果（不出行）；回合 t2：u2（7、8）。
fn walked() -> Walk {
    let mut walk = Walk::default();
    for (message, turn, next_row) in [
        (user("n1"), "", 2),
        (user("n2"), "", 3),
        (user("u1"), "t1", 5),
        (user("a1"), "t1", 7),
        (result("r1", "a1"), "t1", 7),
        (user("u2"), "t2", 9),
    ] {
        walk.enter(&message, turn);
        walk.leave(next_row);
    }
    walk
}

fn ids(records: Vec<MessageId>) -> Vec<String> {
    records
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect()
}

#[test]
fn rows_of_a_turn_read_the_whole_turn_and_turnless_rows_read_their_own_message() {
    let walk = walked();
    // 回合 t1 里的一行（定位用回合头的行号 3）：整个回合，结果归 a1，不重复
    assert_eq!(ids(walk.records([(3, "t1")])), ["u1", "a1"]);
    // 没有回合的行：行号落在哪一条就是哪一条
    assert_eq!(ids(walk.records([(1, "")])), ["n1"]);
    assert_eq!(ids(walk.records([(2, "")])), ["n2"]);
    // 几行一起：按走的先后
    assert_eq!(
        ids(walk.records([(7, "t2"), (0, ""), (3, "t1")])),
        ["n1", "u1", "a1", "u2"]
    );
}
