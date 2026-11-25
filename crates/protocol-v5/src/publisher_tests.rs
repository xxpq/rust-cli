use serde_json::json;

use super::*;
use crate::generated::core_v1::{Frame, FrameBody};

fn patch(items: &[(&str, Value)]) -> BTreeMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn at(epoch: &str, seq: u64) -> Cursor {
    Cursor {
        epoch: epoch.to_owned(),
        seq,
    }
}

#[test]
fn each_change_is_one_batch_and_no_change_is_not_published() {
    let mut p = Publisher::new("agent", "e");
    assert!(p.set_state(patch(&[("phase", json!("starting"))])));
    assert!(!p.set_state(patch(&[("phase", json!("starting"))])));
    assert!(p.set_state(patch(&[("phase", json!("ready")), ("build", json!("1"))])));
    assert_eq!(p.cursor(), at("e", 2));
    assert_eq!(p.state().get("phase"), Some(&json!("ready")));
}

#[test]
fn a_cursor_in_the_window_resumes_and_others_get_a_snapshot() {
    let mut p = Publisher::new("agent", "e");
    p.set_state(patch(&[("phase", json!("starting"))]));
    p.set_state(patch(&[("phase", json!("ready"))]));
    let frame = p.frame_for(Some(&at("e", 1)), "s", false);
    assert_eq!((frame.from, frame.to), (1, 2));
    let FrameBody::Deltas(deltas) = frame.body else {
        panic!("not deltas")
    };
    assert_eq!(
        deltas.deltas,
        vec![json!({"op": "state.set", "patch": {"phase": "ready"}})]
    );
    // 另一个 epoch、没有 cursor、超前的 cursor：快照
    for cursor in [Some(at("other", 1)), None, Some(at("e", 9))] {
        let frame = p.frame_for(cursor.as_ref(), "s", false);
        assert_eq!((frame.from, frame.to), (0, 2));
        assert!(matches!(frame.body, FrameBody::Snapshot(_)));
    }
}

/// 落后到日志之外（以前「256 批之外」）：按变更序号续传，不给快照（01 §19.2）。
#[test]
fn a_cursor_older_than_the_log_resumes_from_the_change_sequence() {
    let mut p = Publisher::new("agent", "e").with_retention(64, 8);
    for n in 0..=300 {
        p.set_state(patch(&[("n", json!(n))]));
    }
    assert!(p.log.floor().is_some_and(|floor| floor > 1));
    let frame = p.frame_for(Some(&at("e", 0)), "s", false);
    assert_eq!((frame.from, frame.to), (0, 301));
    let FrameBody::Deltas(deltas) = frame.body else {
        panic!("not deltas")
    };
    assert_eq!(
        deltas.deltas,
        vec![json!({"op": "state.set", "patch": {"n": 300}})]
    );
}

#[test]
fn frames_serialize_to_the_wire_shape() {
    let mut p = Publisher::new("agent", "e");
    p.set_state(patch(&[("phase", json!("ready"))]));
    assert_eq!(
        serde_json::to_value(p.frame_for(None, "s", false)).unwrap(),
        json!({"topic": "agent", "sub": "s", "epoch": "e", "from": 0, "to": 1,
               "body": {"kind": "snapshot", "snapshot": {"state": {"phase": "ready"}, "collections": {}}}})
    );
}

fn row(id: &str, title: &str) -> (String, Fields) {
    (id.to_owned(), patch(&[("title", json!(title))]))
}

#[test]
fn a_put_batch_is_one_seq_and_its_rows_carry_that_seq_as_v() {
    let mut p = Publisher::new("ws/%2Fa/sessions", "e");
    // 空的一批不占 seq，但集合出现了（快照里是空集合）
    assert!(!p.put("facts", Vec::new()));
    assert_eq!(p.cursor(), at("e", 0));
    assert!(p.put("facts", vec![row("s2", "b"), row("s1", "a")]));
    // 值没变的项去掉；全都没变不发布
    assert!(!p.put("facts", vec![row("s1", "a")]));
    assert!(p.put("facts", vec![row("s1", "a"), row("s2", "b2")]));
    assert_eq!(p.cursor(), at("e", 2));
    let FrameBody::Deltas(deltas) = p.frame_for(Some(&at("e", 1)), "s", false).body else {
        panic!("not deltas")
    };
    assert_eq!(
        deltas.deltas,
        vec![
            json!({"op": "entity.put", "col": "facts", "id": "s2", "v": 2, "data": {"title": "b2"}})
        ]
    );
    // 快照：扁平的 {id, v, ...}，按 id 排序；v 是最后一次变化那一批的 seq
    let FrameBody::Snapshot(snapshot) = p.frame_for(None, "s", false).body else {
        panic!("not a snapshot")
    };
    assert_eq!(
        snapshot.snapshot.collections["facts"],
        vec![
            json!({"id": "s1", "v": 1, "title": "a"}),
            json!({"id": "s2", "v": 2, "title": "b2"}),
        ]
    );
    let mut fresh = Publisher::new("t", "e");
    fresh.put("facts", Vec::new());
    let FrameBody::Snapshot(snapshot) = fresh.frame_for(None, "s", false).body else {
        panic!("not a snapshot")
    };
    assert_eq!(snapshot.snapshot.collections["facts"], Vec::<Value>::new());
}

fn fields(items: &[(&str, Value)]) -> Fields {
    patch(items)
}

fn deltas(frame: Frame) -> Vec<Value> {
    match frame.body {
        FrameBody::Deltas(deltas) => deltas.deltas,
        FrameBody::Snapshot(_) => panic!("not deltas"),
    }
}

/// 一批混合 4 种 op（protocol-v5.md §15.3.6）：一个 seq；append 不改 v；没变的 put / state.set 去掉。
#[test]
fn a_mixed_batch_is_one_seq_and_append_keeps_the_version() {
    let mut p = Publisher::new("ws/a/session/s", "e");
    let mut batch = Batch::default();
    batch.put("rows", "r1", fields(&[("text", json!("he"))]));
    batch.state("rowsWindow", json!({"totalCount": 1}));
    assert!(p.publish(batch));
    let mut batch = Batch::default();
    batch.append("rows", "r1", "text", "llo");
    batch.put("rows", "r2", fields(&[("text", json!(""))]));
    batch.state("rowsWindow", json!({"totalCount": 1}));
    assert!(p.publish(batch));
    assert_eq!(p.cursor(), at("e", 2));
    assert_eq!(
        p.entity("rows", "r1"),
        Some(&fields(&[("text", json!("hello"))]))
    );
    assert_eq!(p.version("rows", "r1"), Some(1));
    assert_eq!(p.version("rows", "r2"), Some(2));
    assert_eq!(
        deltas(p.frame_for(Some(&at("e", 1)), "s", false)),
        vec![
            json!({"op": "entity.append", "col": "rows", "id": "r1", "path": "text", "chunk": "llo"}),
            json!({"op": "entity.put", "col": "rows", "id": "r2", "v": 2, "data": {"text": ""}}),
        ]
    );
    // 没变的 put 与删一个不在的项：什么都不发
    let mut batch = Batch::default();
    batch.put("rows", "r2", fields(&[("text", json!(""))]));
    batch.remove("rows", "r9");
    assert!(!p.publish(batch));
    let mut batch = Batch::default();
    batch.remove("rows", "r2");
    assert!(p.publish(batch));
    assert_eq!(p.entity("rows", "r2"), None);
}

/// 窗口只影响快照（02 §2.3）：按 (序号字段, id) 取尾部；增量照常带窗口外的项。
#[test]
fn the_window_keeps_the_tail_of_the_snapshot_only() {
    let mut p =
        Publisher::new("ws/a/session/s", "e").with_window("rows", "createdAtSeq", 2, usize::MAX);
    for (id, n) in [("c", 0), ("a", 1), ("b", 2)] {
        let mut batch = Batch::default();
        batch.put("rows", id, fields(&[("createdAtSeq", json!(n))]));
        p.publish(batch);
    }
    let FrameBody::Snapshot(snapshot) = p.frame_for(None, "s", false).body else {
        panic!("not a snapshot")
    };
    let ids: Vec<&str> = snapshot.snapshot.collections["rows"]
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(deltas(p.frame_for(Some(&at("e", 0)), "s", false)).len(), 3);
}

/// 快照的尾窗从回合的开头起（protocol-v5.md §71.1）：尾部里有开头时丢掉它前面的半个回合；没有开头（一个回合比窗口长）时照旧按行数；
/// 不属于回合的行（`turnId` 为空）都是开头。
#[test]
fn the_snapshot_window_starts_at_the_first_opening_it_holds() {
    fn opens(item: &Value) -> bool {
        item["kind"] == "turnHeader" || item["turnId"] == ""
    }
    let snapshot_ids = |rows: &[(&str, &str, &str)], limit: usize| -> Vec<String> {
        let mut p = Publisher::new("ws/a/session/s", "e")
            .with_window("rows", "createdAtSeq", limit, usize::MAX)
            .with_openings(opens);
        for (n, (id, kind, turn)) in rows.iter().enumerate() {
            let mut batch = Batch::default();
            batch.put(
                "rows",
                id,
                fields(&[
                    ("createdAtSeq", json!(n)),
                    ("kind", json!(kind)),
                    ("turnId", json!(turn)),
                ]),
            );
            p.publish(batch);
        }
        let FrameBody::Snapshot(snapshot) = p.frame_for(None, "s", false).body else {
            panic!("not a snapshot")
        };
        snapshot.snapshot.collections["rows"]
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let turns = [
        ("h1", "turnHeader", "t1"),
        ("a", "assistantText", "t1"),
        ("b", "toolCall", "t1"),
        ("h2", "turnHeader", "t2"),
        ("c", "assistantText", "t2"),
    ];
    assert_eq!(snapshot_ids(&turns, 4), ["h2", "c"]);
    assert_eq!(snapshot_ids(&turns, 2), ["h2", "c"]);
    assert_eq!(snapshot_ids(&turns, 5), ["h1", "a", "b", "h2", "c"]);
    // 一个回合比窗口长：照旧按行数
    assert_eq!(snapshot_ids(&turns, 1), ["c"]);
    // 不属于回合的行都是开头：照旧按行数
    let turnless = [
        ("x", "userInput", ""),
        ("y", "assistantText", ""),
        ("z", "toolCall", ""),
    ];
    assert_eq!(snapshot_ids(&turnless, 2), ["y", "z"]);
}

/// `adopt`：集合相同 → 只发变了的 state 键；不同 → 同一 epoch 的新快照（seq + 1，清空保留的批）。
#[test]
fn adopting_content_publishes_state_or_resets() {
    let mut p = Publisher::new("ws/a/session/s", "e");
    let mut content = Content::default();
    content
        .state
        .insert("control".to_owned(), json!({"runtime": "cold"}));
    content
        .collections
        .entry("rows".to_owned())
        .or_default()
        .insert("r1".to_owned(), fields(&[("text", json!("x"))]));
    assert!(p.adopt(content.clone()));
    assert_eq!(p.cursor(), at("e", 1));
    content
        .state
        .insert("control".to_owned(), json!({"runtime": "ready"}));
    assert!(!p.adopt(content.clone()));
    assert_eq!(p.cursor(), at("e", 2));
    assert_eq!(
        deltas(p.frame_for(Some(&at("e", 1)), "s", false)),
        vec![json!({"op": "state.set", "patch": {"control": {"runtime": "ready"}}})]
    );
    content
        .collections
        .get_mut("rows")
        .unwrap()
        .insert("r2".to_owned(), fields(&[("text", json!("y"))]));
    assert!(p.adopt(content));
    assert_eq!(p.cursor(), at("e", 3));
    assert!(matches!(
        p.frame_for(Some(&at("e", 2)), "s", false).body,
        FrameBody::Snapshot(_)
    ));
}

fn ids(page: &Page) -> Vec<&str> {
    page.items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect()
}

/// 分页与快照窗口同一个全序（`rows.range`，protocol-v5.md §40.2）：排在 `before` 之前的最后 `limit` 项，升序；`more` = 再往前还有。
#[test]
fn a_page_cuts_the_same_order_as_the_snapshot_window() {
    let mut p =
        Publisher::new("ws/a/session/s", "e").with_window("rows", "createdAtSeq", 2, usize::MAX);
    // 序号相同时按 id：(1, a) 在 (1, b) 之前
    for (id, n) in [("c", 0), ("b", 1), ("a", 1), ("d", 3)] {
        let mut batch = Batch::default();
        batch.put("rows", id, fields(&[("createdAtSeq", json!(n))]));
        p.publish(batch);
    }
    let tail = p.page("rows", "createdAtSeq", None, 2).unwrap();
    assert_eq!((ids(&tail), tail.more), (vec!["b", "d"], true));
    let FrameBody::Snapshot(snapshot) = p.frame_for(None, "s", false).body else {
        panic!("not a snapshot")
    };
    assert_eq!(snapshot.snapshot.collections["rows"], tail.items);
    let older = p.page("rows", "createdAtSeq", Some("b"), 2).unwrap();
    assert_eq!((ids(&older), older.more), (vec!["c", "a"], false));
    // 项是快照里的写法：扁平的 {id, v, ...}
    assert_eq!(
        older.items[0],
        json!({"id": "c", "v": 1, "createdAtSeq": 0})
    );
    let first = p.page("rows", "createdAtSeq", Some("c"), 2).unwrap();
    assert_eq!((first.items.len(), first.more), (0, false));
    let all = p.page("rows", "createdAtSeq", None, usize::MAX).unwrap();
    assert_eq!((ids(&all), all.more), (vec!["c", "a", "b", "d"], false));
    // 不在集合里的 before
    assert_eq!(p.page("rows", "createdAtSeq", Some("z"), 2), None);
}

/// `at` 过期（02 §2.3）：epoch 不同，或早于最后一次新快照；新的批（新行、追加）与只改 state 的 `adopt` 不让它过期。
#[test]
fn a_cursor_goes_stale_on_another_epoch_or_a_newer_snapshot_only() {
    let mut p = Publisher::new("ws/a/session/s", "e");
    let mut content = Content::default();
    content
        .collections
        .entry("rows".to_owned())
        .or_default()
        .insert("r1".to_owned(), fields(&[("text", json!("x"))]));
    p.adopt(content.clone());
    assert!(p.current(&at("e", 1)));
    assert!(!p.current(&at("other", 1)));
    // 第一份快照之前的位置
    assert!(!p.current(&at("e", 0)));
    let mut batch = Batch::default();
    batch.put("rows", "r2", fields(&[("text", json!(""))]));
    p.publish(batch);
    let mut batch = Batch::default();
    batch.append("rows", "r2", "text", "y");
    p.publish(batch);
    content
        .state
        .insert("control".to_owned(), json!({"runtime": "ready"}));
    content
        .collections
        .get_mut("rows")
        .unwrap()
        .insert("r2".to_owned(), fields(&[("text", json!("y"))]));
    assert!(!p.adopt(content.clone()));
    assert_eq!(p.cursor(), at("e", 4));
    assert!(p.current(&at("e", 1)));
    // 换了集合：同一 epoch 的新快照，之前的位置都过期
    content.collections.get_mut("rows").unwrap().remove("r1");
    assert!(p.adopt(content));
    assert_eq!(p.cursor(), at("e", 5));
    assert!(!p.current(&at("e", 4)));
    assert!(p.current(&at("e", 5)));
}
