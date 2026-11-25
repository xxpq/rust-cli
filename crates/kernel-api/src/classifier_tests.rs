use serde_json::json;

use super::*;

/// 空串同对照实现的空投影；缺字段时用原始输入的 JSON 文本（auto-classifier.md §15.3）。
#[test]
fn projections_skip_empty_text_and_fall_back_to_the_raw_input() {
    let input = json!({"command": "ls", "n": 2});
    assert_eq!(
        ClassifierInput::field(&input, "command"),
        ClassifierInput::Text("ls".to_owned())
    );
    assert_eq!(
        ClassifierInput::field(&input, "missing"),
        ClassifierInput::Text(r#"{"command":"ls","n":2}"#.to_owned())
    );
    assert_eq!(
        ClassifierInput::field(&json!({"command": ""}), "command"),
        ClassifierInput::Skip
    );
    assert_eq!(
        ClassifierInput::first_of(&json!({"shell_id": "b1"}), &["task_id", "shell_id"]),
        ClassifierInput::Text("b1".to_owned())
    );
    assert_eq!(
        ClassifierInput::first_of(&json!({}), &["task_id", "shell_id"]),
        ClassifierInput::Skip
    );
}

/// JS 的 `String()`：字符串原样，数字按 JS 的写法，对象写成 JSON。
#[test]
fn js_text_follows_javascript() {
    assert_eq!(js_text(&json!("a b")), "a b");
    assert_eq!(js_text(&json!(300)), "300");
    assert_eq!(js_text(&json!(1.5)), "1.5");
    assert_eq!(js_text(&json!(true)), "true");
    assert_eq!(js_text(&json!(null)), "null");
    assert_eq!(js_text(&json!({"a": [1]})), r#"{"a":[1]}"#);
}

/// 等待时间的说法（auto-classifier.md §15.7、§16.3）。
#[test]
fn waits_read_like_the_reference() {
    assert_eq!(wait_text(0), "about 1 second");
    assert_eq!(wait_text(1_400), "about 1 second");
    assert_eq!(wait_text(30_000), "about 30 seconds");
    assert_eq!(wait_text(119_000), "about 119 seconds");
    assert_eq!(wait_text(150_000), "about 3 minutes");
    assert_eq!(wait_text(3_600_000), "about 1 hour");
    assert_eq!(wait_text(5_400_000), "about 1.5 hours");
    assert_eq!(wait_text(90_000_000), "more than a day");
}
