//! 要写的行的列：id 与会话从对象里取、`data` 去掉这些键，user 的 `model`、assistant 的更新时间、part 的存储形态。
use rust_cli_base::js::json::stringify;
use serde_json::{Value, json};

use super::*;

fn data(map: &Map<String, Value>) -> String {
    stringify(&Value::Object(map.clone()))
}

#[test]
fn a_user_message_row_keeps_key_order_and_gets_the_legacy_model() {
    let info = json!({
        "id": "msg_1", "sessionID": "ses_1", "role": "user",
        "time": {"created": 10},
        "modelSelection": {"providerId": "p", "modelId": "m"},
    });
    let row = message_row(&info, 99).unwrap();
    assert_eq!(
        (row.id, row.session, row.created, row.updated),
        ("msg_1", "ses_1", 10, 10)
    );
    assert_eq!(
        data(&row.data),
        r#"{"role":"user","time":{"created":10},"modelSelection":{"providerId":"p","modelId":"m"},"model":{"providerID":"p","modelID":"m"}}"#
    );
}

#[test]
fn an_assistant_row_is_updated_when_it_completed_or_now() {
    let done = json!({"id": "a", "sessionID": "s", "role": "assistant", "time": {"created": 10, "completed": 25}});
    assert_eq!(message_row(&done, 99).unwrap().updated, 25);
    let running =
        json!({"id": "a", "sessionID": "s", "role": "assistant", "time": {"created": 10}});
    let row = message_row(&running, 99).unwrap();
    assert_eq!(row.updated, 99);
    assert!(!row.data.contains_key("model"));
}

#[test]
fn malformed_records_are_storage_errors_with_the_same_words() {
    let words = |error: StoreError| match error {
        StoreError::Storage { message } => message,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        words(message_row(&json!({"sessionID": "s"}), 1).unwrap_err()),
        "Stored record needs a string id"
    );
    assert_eq!(
        words(
            message_row(
                &json!({"id": "a", "sessionID": "s", "role": "user", "time": {}}),
                1
            )
            .unwrap_err()
        ),
        "Stored message needs time.created"
    );
    assert_eq!(
        words(part_row(&json!({"id": "p", "sessionID": "s"}), 1).unwrap_err()),
        "Stored record needs a string messageID"
    );
}

#[test]
fn a_part_row_strips_its_keys_and_stores_model_changes_and_subtasks_the_node_way() {
    let text = json!({"id": "p1", "messageID": "m1", "sessionID": "s1", "type": "text", "text": "hi", "time": {"start": 7}});
    let row = part_row(&text, 99).unwrap();
    assert_eq!(
        (row.id, row.message, row.session, row.created),
        ("p1", "m1", "s1", 7)
    );
    assert_eq!(
        data(&row.data),
        r#"{"type":"text","text":"hi","time":{"start":7}}"#
    );

    let change = json!({
        "id": "p2", "messageID": "m1", "sessionID": "s1", "type": "timeline", "timelineType": "model_change",
        "fromModel": {"providerId": "a", "modelId": "x"},
        "toModel": {"providerId": "b", "modelId": "y", "options": {"reasoningLevel": "high"}, "label": "Y"},
    });
    assert_eq!(
        data(&part_row(&change, 99).unwrap().data),
        r#"{"type":"timeline","timelineType":"model_change","toModel":{"providerID":"b","modelID":"y","variant":"high","label":"Y"},"fromModelSelection":{"providerId":"a","modelId":"x"},"toModelSelection":{"providerId":"b","modelId":"y","options":{"reasoningLevel":"high"},"label":"Y"}}"#
    );

    let subtask = json!({"id": "p3", "messageID": "m1", "sessionID": "s1", "type": "subtask", "model": {"providerId": "a"}});
    assert_eq!(
        data(&part_row(&subtask, 99).unwrap().data),
        r#"{"type":"subtask","modelSelection":{"providerId":"a"}}"#
    );
}

#[test]
fn legacy_members_of_the_previous_row_are_written_back() {
    let new = part_row(
        &json!({"id": "p", "messageID": "m", "sessionID": "s", "type": "subtask", "prompt": "x"}),
        1,
    )
    .unwrap()
    .data;
    // 已有的键原位替换，没有的追加在末尾；旧行里没有的 legacy 键不加。
    let kept = preserve_legacy(
        r#"{"model":{"old":true},"other":1}"#,
        new.clone(),
        &PART_LEGACY,
    );
    assert_eq!(
        data(&kept),
        r#"{"type":"subtask","prompt":"x","model":{"old":true}}"#
    );
    // 旧行不是 JSON 对象：新数据原样。
    assert_eq!(preserve_legacy("not json", new.clone(), &PART_LEGACY), new);
}
