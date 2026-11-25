//! 窗口外的项换成骨架（protocol-v5.md §67）：Publisher 一侧的规则。会话行的哪些留整行在 `session/projection_residence_tests.rs`。

use serde_json::{Value, json};

use super::*;

const OUTLINE: &[&str] = &["seq", "kind"];

fn row(seq: u64, text: &str) -> Fields {
    Fields::from([
        ("seq".to_owned(), json!(seq)),
        ("kind".to_owned(), json!("k")),
        ("text".to_owned(), json!(text)),
    ])
}

/// 三项、窗口一项：最早的两项可以换成骨架。
fn windowed() -> Publisher {
    let mut publisher = Publisher::new("t", "e").with_window("rows", "seq", 1, usize::MAX);
    publisher.put(
        "rows",
        (0..3).map(|n| (format!("r{n}"), row(n, "body"))).collect(),
    );
    publisher
}

#[test]
fn only_items_outside_the_window_that_are_not_kept_become_outlines() {
    let mut publisher = windowed();
    publisher.thin("rows", OUTLINE, u64::MAX, |fields| fields["text"] == "keep");
    assert_eq!(publisher.entity("rows", "r0"), None);
    assert_eq!(publisher.field("rows", "r0", "kind"), Some(&json!("k")));
    assert_eq!(publisher.field("rows", "r0", "text"), None);
    assert_eq!(publisher.version("rows", "r0"), Some(1));
    // 窗口里的那一项不动
    assert_eq!(publisher.entity("rows", "r2"), Some(&row(2, "body")));
    // 骨架照样排序：分页的顺序不变，标出骨架
    let page = publisher.page("rows", "seq", None, usize::MAX).unwrap();
    let ids: Vec<&Value> = page.items.iter().map(|item| &item["id"]).collect();
    assert_eq!(ids, [&json!("r0"), &json!("r1"), &json!("r2")]);
    assert_eq!(page.outlined, ["r0", "r1"]);
}

#[test]
fn a_put_brings_an_outline_back_to_a_whole_item() {
    let mut publisher = windowed();
    publisher.thin("rows", OUTLINE, u64::MAX, |_| false);
    assert_eq!(publisher.entity("rows", "r0"), None);
    assert!(publisher.put("rows", vec![("r0".to_owned(), row(0, "body"))]));
    assert_eq!(publisher.entity("rows", "r0"), Some(&row(0, "body")));
}

#[test]
fn a_collection_without_a_window_is_left_whole() {
    let mut publisher = Publisher::new("t", "e");
    publisher.put("rows", vec![("r0".to_owned(), row(0, "body"))]);
    publisher.thin("rows", OUTLINE, u64::MAX, |_| false);
    assert_eq!(publisher.entity("rows", "r0"), Some(&row(0, "body")));
}

/// 只换每个在订订阅都已收到的项（protocol-v5.md §68.0 第 5 条）：落后的订阅续传时要从当前内容拼它的 put，骨架拼不出来。
/// 换了之后 `lossy` 抬到它的变更序号：落在它之前、又已不在日志里的 cursor 只能拿快照。
#[test]
fn an_item_a_subscription_has_not_received_stays_whole() {
    let mut publisher = windowed();
    let sent = publisher.cursor().seq;
    // r0 在窗口外、发出之后又改了一次：订阅只收到改之前的那一份
    publisher.put("rows", vec![("r0".to_owned(), row(0, "changed"))]);
    publisher.thin("rows", OUTLINE, sent, |_| false);
    assert_eq!(publisher.entity("rows", "r0"), Some(&row(0, "changed")));
    assert_eq!(publisher.entity("rows", "r1"), None);
    // 订阅都收到了这一次改动：可以换
    publisher.thin("rows", OUTLINE, publisher.cursor().seq, |_| false);
    assert_eq!(publisher.entity("rows", "r0"), None);
}
