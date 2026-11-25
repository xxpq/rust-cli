//! 范围读（node-store.md §5.6）：读全部 = 冷读取；从尾往前一段一段读，两边（只供显示的、模型侧的）各自拼起来 = 冷读取；
//! 段首是回合开头；`Calling` 给出调用过那个工具的回合；`floor` 不在分支上时 NotFound。

use std::collections::HashSet;

use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::TurnOrigin;
use rust_cli_kernel_api::store::{
    Fact, HistoryRange, HistorySpan, LoadedSession, SessionStore, StoreError, StoreQueries,
    TurnRecord,
};
use serde_json::json;

use crate::hydrate_tests::{database, fixture};
use crate::tests::{TestStore, assistant, batch, started, text, user};
use rust_cli_node_records::range::HistoryIndex;
use rust_cli_node_records::rows::Rows;

const CASES: [&str; 5] = ["basic", "compacted", "rewound", "rich", "failures"];

fn before(floor: Option<&MessageId>, records: usize) -> HistorySpan {
    HistorySpan::Before {
        floor: floor.cloned(),
        records,
    }
}

/// 不带历史索引读一段（每次建索引）。
fn read(rows: &dyn Rows, session: &SessionId, span: &HistorySpan) -> HistoryRange {
    rust_cli_node_records::range::read(rows, session, span, None)
        .unwrap()
        .range
}

/// 一段里的内核消息所在的存储消息。
fn records(loaded: &LoadedSession) -> HashSet<MessageId> {
    loaded
        .earlier
        .iter()
        .chain(&loaded.messages)
        .map(|message| message.record().clone())
        .collect()
}

/// 读全部（`records` 不设上限）与冷读取逐项相同：两边的消息（保留段的副本照样改了用量）、回合记录、输入记录、计划、会话级的事实；按消息的事实只差
/// 回退掉的分支上的那些（冷读取读全部消息记录，范围读只读分支上的）；子代理同 `subagents::stored`。
#[test]
fn a_range_over_everything_matches_the_cold_load() {
    let fixture = fixture();
    for case in CASES {
        let (_dir, conn) = database(&fixture[case]["tables"]);
        let session = SessionId::new(fixture[case]["sessionID"].as_str().unwrap());
        let rows = crate::rows::Sqlite(&conn);
        let (loaded, _) = rust_cli_node_records::load::load(&rows, &session).unwrap();
        let range = read(&rows, &session, &before(None, usize::MAX));
        assert!(!range.earlier, "{case}");
        let got = &range.loaded;
        assert_eq!(got.messages, loaded.messages, "{case}");
        assert_eq!(got.earlier, loaded.earlier, "{case}");
        assert_eq!(got.turns, loaded.turns, "{case}");
        assert_eq!(got.inputs, loaded.inputs, "{case}");
        assert_eq!(got.plans, loaded.plans, "{case}");
        assert_eq!(
            (
                &got.meta,
                &got.model,
                got.mode,
                got.plan,
                &got.turn,
                got.last_turn_failed
            ),
            (
                &loaded.meta,
                &loaded.model,
                loaded.mode,
                loaded.plan,
                &loaded.turn,
                loaded.last_turn_failed
            ),
            "{case}"
        );
        let shown = records(got);
        let calls: HashSet<&ToolUseId> = got
            .earlier
            .iter()
            .chain(&got.messages)
            .flat_map(Message::tool_uses)
            .collect();
        // 来源：冷读取读全部消息记录（含回退掉的分支），范围读只读分支上的（含不成内核消息的那几条）
        assert!(
            got.sources
                .iter()
                .all(|source| loaded.sources.contains(source)),
            "{case}"
        );
        assert!(
            loaded
                .sources
                .iter()
                .filter(|source| shown.contains(&source.message))
                .all(|source| got.sources.contains(source)),
            "{case}"
        );
        let heads: Vec<_> = loaded
            .output_heads
            .iter()
            .filter(|head| calls.contains(&head.tool_use))
            .cloned()
            .collect();
        assert_eq!(got.output_heads, heads, "{case}");
        let times: Vec<_> = loaded
            .tool_times
            .iter()
            .filter(|time| calls.contains(&time.tool_use))
            .cloned()
            .collect();
        assert_eq!(got.tool_times, times, "{case}");
        assert_eq!(
            range.subagents,
            rust_cli_node_records::subagents::stored(&rows, &session).unwrap(),
            "{case}"
        );
    }
}

/// 按存储的消息分组：每条存储消息换出的内核消息（同一条里的先后不变）。
fn grouped<'a>(
    messages: impl Iterator<Item = &'a Message>,
) -> std::collections::BTreeMap<String, Vec<Message>> {
    let mut groups: std::collections::BTreeMap<String, Vec<Message>> = Default::default();
    for message in messages {
        groups
            .entry(message.record().as_str().to_owned())
            .or_default()
            .push(message.clone());
    }
    groups
}

/// 从尾往前每次读 `width` 条：段两两不相交、合起来是读全部的那些存储消息，每条换出的内核消息与读全部的相同（保留段的副本照样改了用量）；
/// 最后一段 `earlier` 为假，之前的都为真；下一段的 `floor` 是这一段的段首。段在显示顺序里切，压缩点的保留段与摘要可以分在两段
/// （拼回去的先后由建投影时的 `stored_order` 定，与读全部相同，见前端的对拍）。
#[test]
fn reading_back_one_range_at_a_time_covers_the_cold_load() {
    let fixture = fixture();
    for case in CASES {
        let (_dir, conn) = database(&fixture[case]["tables"]);
        let session = SessionId::new(fixture[case]["sessionID"].as_str().unwrap());
        let rows = crate::rows::Sqlite(&conn);
        let whole = read(&rows, &session, &before(None, usize::MAX));
        for width in [1, 2, 3, 5] {
            let mut pages: Vec<HistoryRange> = Vec::new();
            let mut floor: Option<MessageId> = None;
            loop {
                let page = read(&rows, &session, &before(floor.as_ref(), width));
                let more = page.earlier;
                floor.clone_from(&page.floor);
                pages.push(page);
                if !more {
                    break;
                }
                assert!(pages.len() < 100, "{case} {width}: 翻不到头");
            }
            let mut seen: HashSet<MessageId> = HashSet::new();
            for page in &pages {
                let mine = records(&page.loaded);
                assert!(mine.is_disjoint(&seen), "{case} {width}: 段重叠");
                seen.extend(mine);
            }
            assert_eq!(seen, records(&whole.loaded), "{case} {width}");
            let all =
                |loaded: &LoadedSession| grouped(loaded.earlier.iter().chain(&loaded.messages));
            let mut joined = std::collections::BTreeMap::new();
            for page in &pages {
                joined.extend(all(&page.loaded));
            }
            assert_eq!(joined, all(&whole.loaded), "{case} {width}");
            assert!(
                pages.iter().all(|page| page.floor.is_some()),
                "{case} {width}"
            );
        }
    }
}

fn record(turn: &str, first: &str) -> Fact {
    TurnRecord::opened(
        TurnId::new(turn),
        MessageId::new(first),
        TurnOrigin::Input,
        1,
    )
    .fact()
}

fn tool_result(id: &str, call: &str, assistant: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(call),
                content: vec![ToolResultContent::Text { text: "a".into() }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(assistant),
            },
        }),
    }
}

/// 本工程写的三个回合：段首补到回合开头；回合记录、工具起止只带段里的；`Calling` 给出调用过那个工具的整个回合；`floor` 不在分支上 → NotFound。
#[tokio::test]
async fn ranges_of_written_turns_start_at_turn_openings() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let call = ContentBlock::ToolUse {
        id: ToolUseId::new("toolu_1"),
        name: "Read".into(),
        input: json!({"file_path": "/w/a"}),
    };
    let turns = [
        vec![
            user("msg_u1", "one"),
            record("turn_1", "msg_u1"),
            assistant("msg_a1", vec![call], None),
            tool_result("msg_r1", "toolu_1", "msg_a1"),
            assistant("msg_a1b", vec![text("done")], None),
        ],
        vec![
            user("msg_u2", "two"),
            record("turn_2", "msg_u2"),
            assistant("msg_a2", vec![text("second")], None),
        ],
        vec![
            user("msg_u3", "three"),
            record("turn_3", "msg_u3"),
            assistant("msg_a3", vec![text("third")], None),
        ],
    ];
    let session = SessionId::new("sess_1");
    for facts in turns {
        t.store.commit(&session, batch(None, facts)).await.unwrap();
    }
    let ids = |range: &HistoryRange| -> Vec<String> {
        let mut ids: Vec<String> = range
            .loaded
            .messages
            .iter()
            .map(|m| m.record().as_str().to_owned())
            .collect();
        ids.dedup();
        ids
    };
    let turns_of = |range: &HistoryRange| -> Vec<String> {
        range
            .loaded
            .turns
            .iter()
            .map(|turn| turn.turn.as_str().to_owned())
            .collect()
    };
    // 最后一条在回合 3 里：补到它的开头
    let last = t.store.range(&session, before(None, 1)).await.unwrap();
    assert_eq!(ids(&last), ["msg_u3", "msg_a3"]);
    assert_eq!(turns_of(&last), ["turn_3"]);
    assert!(last.earlier);
    assert_eq!(last.floor.as_ref().map(MessageId::as_str), Some("msg_u3"));
    // 往前两条正好是回合 2：段首就是开头
    let second = t
        .store
        .range(&session, before(last.floor.as_ref(), 2))
        .await
        .unwrap();
    assert_eq!(turns_of(&second), ["turn_2"]);
    assert!(second.earlier);
    // 往前三条（第一条落在回合 1 的中间）：补到回合 1 的开头，回合 2 整个在里面
    let page = t
        .store
        .range(&session, before(last.floor.as_ref(), 3))
        .await
        .unwrap();
    assert_eq!(page.floor.as_ref().map(MessageId::as_str), Some("msg_u1"));
    assert_eq!(turns_of(&page), ["turn_1", "turn_2"]);
    assert!(!page.earlier);
    assert!(
        page.loaded
            .tool_times
            .iter()
            .all(|time| time.tool_use.as_str() == "toolu_1")
    );
    // Calling：调用过 Read 的那条（msg_a1）所在的整个回合，只取回合 3 的开头之前的；库里按 JSON 成员筛 part
    let calling = |before: Option<&str>| HistorySpan::Calling {
        tool: "Read".to_owned(),
        before: before.map(MessageId::new),
    };
    let around = t
        .store
        .range(&session, calling(Some("msg_u3")))
        .await
        .unwrap();
    assert_eq!(ids(&around), ["msg_u1", "msg_a1", "msg_a1b"]);
    assert!(!around.earlier);
    let none = t
        .store
        .range(&session, calling(Some("msg_u1")))
        .await
        .unwrap();
    assert!(none.loaded.messages.is_empty() && none.floor.is_none());
    let gone = t
        .store
        .range(&session, before(Some(&MessageId::new("msg_gone")), 2))
        .await;
    assert!(matches!(gone, Err(StoreError::NotFound { .. })), "{gone:?}");
    // 整段读的 load 不受影响
    let loaded = t.store.load(&session).await.unwrap();
    assert_eq!(loaded.turns.len(), 3);
}

/// 历史索引（node-store.md §5.7）：库没变时带着上一次读给出的索引读下一段，结果与每次重建索引的读逐项相同，用的就是交回去的那一份。
#[test]
fn pages_read_with_the_earlier_index_match_fresh_reads() {
    let fixture = fixture();
    for case in CASES {
        let (_dir, conn) = database(&fixture[case]["tables"]);
        let session = SessionId::new(fixture[case]["sessionID"].as_str().unwrap());
        let rows = crate::rows::Sqlite(&conn);
        for width in [1, 2, 3, usize::MAX] {
            let mut floor: Option<MessageId> = None;
            let mut index = None;
            loop {
                let span = before(floor.as_ref(), width);
                let fresh =
                    rust_cli_node_records::range::read(&rows, &session, &span, None).unwrap();
                let reused =
                    rust_cli_node_records::range::read(&rows, &session, &span, index.clone())
                        .unwrap();
                assert_eq!(reused.range, fresh.range, "{case} {width}");
                assert_eq!(reused.index, fresh.index, "{case} {width}");
                if let Some(given) = &index {
                    assert!(
                        std::sync::Arc::ptr_eq(given, &reused.index),
                        "{case} {width}"
                    );
                }
                index = Some(reused.index);
                floor.clone_from(&fresh.range.floor);
                if !fresh.range.earlier {
                    break;
                }
            }
        }
        // Calling 也一样
        let span = HistorySpan::Calling {
            tool: "Read".to_owned(),
            before: None,
        };
        let fresh = rust_cli_node_records::range::read(&rows, &session, &span, None).unwrap();
        let reused =
            rust_cli_node_records::range::read(&rows, &session, &span, Some(fresh.index.clone()))
                .unwrap();
        assert_eq!(reused.range, fresh.range, "{case}");
    }
}

/// 这个会话在库的当前版本下记住的历史索引（node-store.md §5.7）。
async fn remembered(t: &TestStore, session: &SessionId) -> Option<std::sync::Arc<HistoryIndex>> {
    let (session, memo) = (session.clone(), t.store.memo.clone());
    t.store
        .with_db(move |conn, _| {
            let version = crate::memo::Version::of(conn).map_err(crate::storage)?;
            Ok(memo.index(&session, version))
        })
        .await
        .unwrap()
}

/// 记住的索引（node-store.md §5.7）：第二段用上一段记下的那一份；本进程写了、别的连接写了，都不再用，重读看得到新消息。
#[tokio::test]
async fn a_remembered_index_is_reused_until_the_database_changes() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    for turn in 1..=3 {
        let facts = vec![
            user(&format!("msg_u{turn}"), "ask"),
            record(&format!("turn_{turn}"), &format!("msg_u{turn}")),
            assistant(&format!("msg_a{turn}"), vec![text("answer")], None),
        ];
        t.store.commit(&session, batch(None, facts)).await.unwrap();
    }
    assert!(remembered(&t, &session).await.is_none());
    let last = t.store.range(&session, before(None, 1)).await.unwrap();
    let first = remembered(&t, &session).await.expect("记下了");
    t.store
        .range(&session, before(last.floor.as_ref(), 1))
        .await
        .unwrap();
    let again = remembered(&t, &session).await.expect("还在");
    assert!(std::sync::Arc::ptr_eq(&first, &again), "第二段没有重建");
    // 子代理目录同一个库版本只读一次（node-store.md §5.7）：读过之后记在同一个版本下，第二次的答案是它
    let listed = t.store.subagents(&session).await.unwrap();
    let (key, memo) = (session.clone(), t.store.memo.clone());
    let kept = t
        .store
        .with_db(move |conn, _| {
            let version = crate::memo::Version::of(conn).map_err(crate::storage)?;
            Ok(memo.subagents(&key, version))
        })
        .await
        .unwrap();
    assert_eq!(kept.as_ref(), Some(&listed));
    assert_eq!(t.store.subagents(&session).await.unwrap(), listed);
    assert!(
        remembered(&t, &session).await.is_some(),
        "读子代理不作废索引"
    );
    // 本进程写
    let facts = vec![
        user("msg_u4", "more"),
        record("turn_4", "msg_u4"),
        assistant("msg_a4", vec![text("fourth")], None),
    ];
    t.store.commit(&session, batch(None, facts)).await.unwrap();
    assert!(
        remembered(&t, &session).await.is_none(),
        "本进程写了之后不用"
    );
    let after = t.store.range(&session, before(None, 1)).await.unwrap();
    assert!(records(&after.loaded).contains(&MessageId::new("msg_u4")));
    // 别的连接写（Node 进程）
    let other = t.connection();
    crate::repo::messages::save_message(
        &other,
        &json!({"id": "msg_n5", "sessionID": "sess_1", "role": "user", "time": {"created": 5000}}),
        5000,
    )
    .unwrap();
    crate::repo::messages::save_part(
        &other,
        &json!({"id": "prt_n5", "messageID": "msg_n5", "sessionID": "sess_1", "type": "text", "text": "from node"}),
        5000,
    )
    .unwrap();
    assert!(
        remembered(&t, &session).await.is_none(),
        "别的连接写了之后不用"
    );
    let after = t.store.range(&session, before(None, 1)).await.unwrap();
    assert!(records(&after.loaded).contains(&MessageId::new("msg_n5")));
}

/// 没有回合记录的历史（node-store.md §5.6）：段首往前补到开回合的输入，每一段从一条输入（或会话的开头）起。段首 = 显示顺序里的第一条存储消息
/// （`floor`；保留段的副本在显示顺序里排在摘要之前，在模型侧的历史里排在它之后）。
#[test]
fn ranges_of_unrecorded_history_start_at_an_input() {
    use rust_cli_kernel_api::turn_walk::opens_turn;
    let fixture = fixture();
    for case in CASES {
        let (_dir, conn) = database(&fixture[case]["tables"]);
        let session = SessionId::new(fixture[case]["sessionID"].as_str().unwrap());
        let rows = crate::rows::Sqlite(&conn);
        let whole = read(&rows, &session, &before(None, usize::MAX));
        let mut floor: Option<MessageId> = None;
        let mut pages = 0;
        loop {
            let page = read(&rows, &session, &before(floor.as_ref(), 1));
            let start = page.floor.clone().expect("一段至少一条");
            let opens = page
                .loaded
                .earlier
                .iter()
                .chain(&page.loaded.messages)
                .any(|message| message.record() == &start && opens_turn(message));
            assert!(
                opens || start == *whole.floor.as_ref().unwrap(),
                "{case}: 段首 {} 不是输入",
                start.as_str()
            );
            pages += 1;
            floor = Some(start);
            if !page.earlier {
                break;
            }
        }
        assert!(pages >= 2, "{case}: 只有一段，没测到补段首");
    }
}
