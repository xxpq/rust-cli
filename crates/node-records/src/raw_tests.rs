//! 冷读取的骨架（node-store.md §5.4）：part 只读出 `type`，读法与 `serde_json::Value` 相同；compaction 的 part 整份解码，同整份解码的结果。

use serde_json::{Value, json};

use super::{RawPart, RawRecord};

fn record(parts: &[&str]) -> RawRecord {
    RawRecord {
        info: json!({"id": "msg_1", "sessionID": "sess_1", "role": "user"}),
        session: "sess_1".to_owned(),
        parts: parts
            .iter()
            .enumerate()
            .map(|(index, data)| RawPart {
                id: format!("prt_{index}"),
                data: (*data).to_owned(),
            })
            .collect(),
    }
}

/// 整份解码之后，哪些 part 的 `type` 是 compaction（`Value` 的读法）。
fn by_value(record: &RawRecord) -> Vec<Value> {
    record
        .decode()
        .unwrap()
        .parts
        .into_iter()
        .filter(|part| part["type"] == "compaction")
        .collect()
}

/// 骨架只留 compaction 的 part，且与整份解码出的那一份相同；别的读法（不是对象、type 不是字符串、键转义、重复的键）与 `Value` 一致。
#[test]
fn the_skeleton_keeps_exactly_the_compaction_parts() {
    let record = record(&[
        r#"{"type": "text", "text": "hi", "compactBoundary": {"x": 1}}"#,
        r#"{"type": "compaction", "compactBoundary": {"preservedSegment": {"headMessageId": "a"}}}"#,
        r#"["compaction"]"#,
        r#""compaction""#,
        r#"{"type": ["compaction"]}"#,
        r#"{"type": "compaction", "n": 1}"#,
        r#"{"type": "compaction", "type": "text"}"#,
        r#"{"type": "text", "type": "compaction", "nested": {"type": "text"}}"#,
        r#"{"nested": {"type": "compaction"}}"#,
        "null",
        "42",
    ]);
    let skeleton = record.skeleton().unwrap();
    assert_eq!(skeleton.info, record.info);
    let expected = by_value(&record);
    let ids: Vec<&str> = expected
        .iter()
        .map(|part| part["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["prt_1", "prt_5", "prt_7"]);
    assert_eq!(skeleton.parts, expected);
}

/// data 不是合法 JSON：骨架与整份解码一样报错（以前读库时就报）。
#[test]
fn a_broken_part_fails_the_skeleton_like_the_full_decode() {
    let record = record(&[r#"{"type": "text""#]);
    assert!(record.skeleton().is_err());
    assert!(record.decode().is_err());
}

/// 消息头（node-store.md §5.6）：role 与 summary 的读法同 `decode_message` 之后读这两个成员。
#[test]
fn a_message_head_reads_role_and_summary_like_the_decoded_info() {
    use super::MessageHead;
    use crate::branch::truthy;
    use crate::codecs::decode_message;
    let cases = [
        r#"{"role":"user","time":{"created":1}}"#,
        r#"{"role":"assistant","tokens":{"input":1},"finish":"stop"}"#,
        r#"{"role":"user","summary":{"title":"Compact summary","body":"x"}}"#,
        r#"{"role":"user","summary":true}"#,
        r#"{"role":"user","summary":false}"#,
        r#"{"role":"user","summary":""}"#,
        r#"{"role":"user","summary":"yes"}"#,
        r#"{"role":"user","summary":0}"#,
        r#"{"role":"user","summary":0.5}"#,
        r#"{"role":"user","summary":null}"#,
        r#"{"role":"user","summary":[]}"#,
        r#"{"role":1,"summary":{}}"#,
        r#"{"role":"user","role":"assistant"}"#,
        r#"{"summary":true,"role":"user","summary":false}"#,
        r#"{"role":"user","summary":true}"#,
        r#"{"nested":{"role":"user","summary":true}}"#,
        r#"[{"role":"user"}]"#,
        r#""user""#,
        "1",
        "null",
        "true",
    ];
    for data in cases {
        let info = decode_message(data, "msg_1", "sess_1").unwrap();
        let head = MessageHead::parse("msg_1".to_owned(), data).unwrap();
        assert_eq!(
            head,
            MessageHead {
                id: "msg_1".to_owned(),
                role: info["role"].as_str().map(str::to_owned),
                summary: truthy(info.get("summary")),
            },
            "{data}"
        );
    }
    for bad in [r#"{"role":"user""#, "{", r#"{"role":"user",}"#] {
        assert!(decode_message(bad, "msg_1", "sess_1").is_err(), "{bad}");
        assert!(
            MessageHead::parse("msg_1".to_owned(), bad).is_err(),
            "{bad}"
        );
    }
}
