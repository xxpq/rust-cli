//! 投影的一次改动（protocol-v5.md §66.5）：建投影的改动只收内容、不记 op（每一项只存一份）；发布用的改动照旧记 op；读不复制，读得到同一步里刚写的与底子里的项。

use serde_json::json;

use super::*;

fn row(text: &str) -> Fields {
    Fields::from([("text".to_owned(), json!(text))])
}

#[test]
fn a_building_edit_keeps_the_content_and_records_no_op() {
    let mut building = Edit::building();
    building.put(ROWS, "r1", row("one"));
    building.append(ROWS, "r1", "text", "+");
    building.state("meta", json!({"title": "t"}));
    assert_eq!(building.get(ROWS, "r1"), Some(&row("one+")));
    let content = building.into_content();
    assert_eq!(content.collections[ROWS]["r1"], row("one+"));
    assert_eq!(content.state["meta"], json!({"title": "t"}));
    // 同样的改动：建投影的没有 op 可发布
    let mut building = Edit::building();
    building.put(ROWS, "r1", row("one"));
    let mut publisher = Publisher::new("ws/a/session/s", "e");
    assert!(!publisher.publish(building.into_batch()));
}

#[test]
fn a_publishing_edit_reads_its_base_and_records_every_op() {
    let mut publisher = Publisher::new("ws/a/session/s", "e");
    publisher.put(ROWS, vec![("r1".to_owned(), row("base"))]);
    let mut edit = Edit::new(Some(&publisher));
    // 底子里的项直接读到（不复制）；要改的地方复制一份再写
    assert_eq!(edit.get(ROWS, "r1"), Some(&row("base")));
    let mut changed = edit.get(ROWS, "r1").cloned().unwrap_or_default();
    changed.insert("text".to_owned(), json!("changed"));
    edit.put(ROWS, "r1", changed);
    assert_eq!(edit.get(ROWS, "r1"), Some(&row("changed")));
    edit.remove(ROWS, "r1");
    assert_eq!(edit.get(ROWS, "r1"), None);
    let batch = edit.into_batch();
    let mut target = Publisher::new("ws/a/session/s", "e");
    target.put(ROWS, vec![("r1".to_owned(), row("base"))]);
    assert!(target.publish(batch));
    assert_eq!(target.entity(ROWS, "r1"), None);
}

#[test]
fn update_changes_an_item_in_place_and_publishes_the_whole_item() {
    // 建投影：就地改，内容里只有改完的那一份
    let mut building = Edit::building();
    building.put(ROWS, "r1", row("one"));
    building.update(ROWS, "r1", |fields| {
        fields.insert("endedAt".to_owned(), json!(2));
    });
    building.update(ROWS, "missing", |_| {
        unreachable!("an absent item is left alone")
    });
    let content = building.into_content();
    assert_eq!(content.collections[ROWS]["r1"]["endedAt"], json!(2));
    assert!(!content.collections[ROWS].contains_key("missing"));
    // 发布：改的是这一步的副本，底子不动；op 带整项
    let mut publisher = Publisher::new("ws/a/session/s", "e");
    publisher.put(ROWS, vec![("r1".to_owned(), row("base"))]);
    let mut edit = Edit::new(Some(&publisher));
    edit.update(ROWS, "r1", |fields| {
        fields.insert("endedAt".to_owned(), json!(2));
    });
    assert_eq!(
        edit.get(ROWS, "r1").map(|fields| fields["endedAt"].clone()),
        Some(json!(2))
    );
    let batch = edit.into_batch();
    assert_eq!(publisher.entity(ROWS, "r1"), Some(&row("base")));
    assert!(publisher.publish(batch));
    let mut expected = row("base");
    expected.insert("endedAt".to_owned(), json!(2));
    assert_eq!(publisher.entity(ROWS, "r1"), Some(&expected));
}
