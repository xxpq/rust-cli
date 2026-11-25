//! 没有回合记录的历史按用户输入分回合（node-store.md §5.6），与 v4 的冷读对拍：`fixtures/node-cold.json` 里五种 Node 写的会话带 v4 投影的行
//! （`turnId` 是产品回合：有消息 id 的输入开的回合就是这条输入的消息 id）。冷读出的内核消息经 kernel-api 的 `TurnWalk` 归到回合，每一行所在的存储消息
//! 的回合 = v4 那一行的 `turnId`。比的是消息成的行（输入、正文、思考、工具调用）；回合头与时间线标记的位置另有规则，不在这里比。
//! v4 另有两种开轮这里认不出（node-store.md §5.6 的差异），逐条列在 [`DIFFERENT`]。

use std::collections::HashMap;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::turn_walk::{Opened, TurnWalk, UNRECORDED_TURN_PREFIX};

use crate::hydrate_tests::{database, fixture};

const CASES: [&str; 5] = ["basic", "compacted", "rewound", "rich", "failures"];

/// 与 v4 不同的行（会话、行的 entityId、v4 的回合、本工程的回合）：
/// - rich 的 `g1` 是 Node 的插话（guide）：v4 并进当前轮；它在内核消息里与普通输入是同一种，本工程另起一轮，之后的 `a2` 跟着它；
/// - rich 的 `a5` 在后台通知唤醒的那一轮（v4 的 `bg`）：Node 的通知消息只有合成 part，不成内核消息，`a5` 留在上一轮。
const DIFFERENT: [(&str, &str, &str, &str); 3] = [
    ("rich", "g1", "u1", "g1"),
    ("rich", "a2", "u1", "g1"),
    ("rich", "a5", "bg", "u2"),
];

/// 冷读出的历史里每条存储消息所在的回合（开它的输入的消息 id；不属于回合为空串），与每个工具调用所在的存储消息。
fn walked(case: &str) -> (HashMap<String, String>, HashMap<String, String>) {
    let fixture = fixture();
    let (_dir, conn) = database(&fixture[case]["tables"]);
    let session = SessionId::new(fixture[case]["sessionID"].as_str().unwrap());
    let (loaded, _) =
        rust_cli_node_records::load::load(&crate::rows::Sqlite(&conn), &session).unwrap();
    assert!(loaded.turns.is_empty(), "{case}: Node 写的会话没有回合记录");
    let mut walk = TurnWalk::new(&loaded.turns);
    let mut current = String::new();
    let (mut turns, mut calls) = (HashMap::new(), HashMap::new());
    for message in loaded.earlier.iter().chain(&loaded.messages) {
        match walk.step(message) {
            Some(Opened::Unrecorded(turn)) => {
                current = turn
                    .as_str()
                    .trim_start_matches(UNRECORDED_TURN_PREFIX)
                    .to_owned();
            }
            Some(Opened::Recorded(_)) => unreachable!("{case}: 没有回合记录"),
            None => {}
        }
        turns
            .entry(message.record().as_str().to_owned())
            .or_insert_with(|| current.clone());
        if let Message::Assistant(assistant) = message {
            for block in &assistant.content {
                if let ContentBlock::ToolUse { id, .. } = block {
                    calls.insert(id.as_str().to_owned(), assistant.id.as_str().to_owned());
                }
            }
        }
    }
    (turns, calls)
}

#[test]
fn unrecorded_turns_match_the_v4_cold_read() {
    let fixture = fixture();
    let mut compared = 0;
    let mut different = Vec::new();
    for case in CASES {
        let (turns, calls) = walked(case);
        for row in fixture[case]["rows"].as_array().unwrap() {
            let entity = row["entityId"].as_str().unwrap();
            let record = match row["kind"].as_str().unwrap() {
                "userInput" | "assistantText" | "reasoning" => entity.to_owned(),
                "toolCall" => match calls.get(entity) {
                    Some(record) => record.clone(),
                    None => continue,
                },
                _ => continue,
            };
            // 不成内核消息的不比（basic 的 `u3` 是还没接入的共享上下文，冷读跳过它）
            let Some(ours) = turns.get(&record) else {
                continue;
            };
            let theirs = row["turnId"].as_str().unwrap();
            compared += 1;
            if ours != theirs {
                different.push((case, entity.to_owned(), theirs.to_owned(), ours.clone()));
            }
        }
    }
    let expected: Vec<(&str, String, String, String)> = DIFFERENT
        .iter()
        .map(|(case, entity, theirs, ours)| {
            (
                *case,
                (*entity).to_owned(),
                (*theirs).to_owned(),
                (*ours).to_owned(),
            )
        })
        .collect();
    assert_eq!(different, expected);
    // 五种会话里消息成的行共 40 行（输入 15、正文 15、思考 1、工具调用 9），除 basic 的 `u3` 都比到了（夹具变了要重数）
    assert_eq!(compared, 39);
}
