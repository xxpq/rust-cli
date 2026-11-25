//! 表单的校验（docs/specs/rust-cli/ext.md §3.13.3）：规则与文字。

use serde_json::{Map, Value, json};

use super::*;

fn field(name: &str, required: bool, kind: FieldKind) -> FormField {
    FormField {
        name: name.to_owned(),
        title: None,
        description: None,
        required,
        kind,
    }
}

fn text(format: Option<TextFormat>, min: Option<u32>, max: Option<u32>) -> FieldKind {
    FieldKind::Text {
        format,
        min_length: min,
        max_length: max,
        default: None,
    }
}

fn number(integer: bool, minimum: Option<f64>, maximum: Option<f64>) -> FieldKind {
    FieldKind::Number {
        integer,
        minimum,
        maximum,
        default: None,
    }
}

fn choices(values: &[&str]) -> Vec<Choice> {
    values
        .iter()
        .map(|value| Choice {
            value: (*value).to_owned(),
            label: value.to_uppercase(),
        })
        .collect()
}

fn check(kind: FieldKind, value: Value) -> Result<Value, String> {
    field("f", false, kind).check(&value)
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

/// 文本的长度与格式的原因；几条问题用 `; ` 接起；长度按 UTF-16 码元。
#[test]
fn text_messages_are_the_reference_texts() {
    assert_eq!(
        check(text(None, Some(3), None), json!("ab")),
        Err("Must be at least 3 characters".to_owned())
    );
    assert_eq!(
        check(text(None, None, Some(1)), json!("ab")),
        Err("Must be at most 1 character".to_owned())
    );
    // 一个表情是两个 UTF-16 码元。
    assert_eq!(
        check(text(None, None, Some(1)), json!("😀")),
        Err("Must be at most 1 character".to_owned())
    );
    assert_eq!(
        check(
            text(Some(TextFormat::Email), Some(30), None),
            json!("not-an-email")
        ),
        Err(
            "Must be at least 30 characters; Must be a valid email address, e.g. user@example.com"
                .to_owned()
        )
    );
    assert_eq!(
        check(
            text(Some(TextFormat::Email), None, None),
            json!("dev@example.com")
        ),
        Ok(json!("dev@example.com"))
    );
    for bad in [
        ".dev@example.com",
        "a..b@example.com",
        "dev@example",
        "dev.@example.com",
    ] {
        assert!(
            check(text(Some(TextFormat::Email), None, None), json!(bad)).is_err(),
            "{bad}"
        );
    }
    assert_eq!(
        check(
            text(Some(TextFormat::Uri), None, None),
            json!("example.com")
        ),
        Err("Must be a valid URI, e.g. https://example.com".to_owned())
    );
    assert!(
        check(
            text(Some(TextFormat::Uri), None, None),
            json!("https://example.com/a?b")
        )
        .is_ok()
    );
    assert_eq!(
        check(
            text(Some(TextFormat::Date), None, None),
            json!("2024-02-30")
        ),
        Err("Must be a valid date, e.g. 2024-03-15".to_owned())
    );
    assert!(
        check(
            text(Some(TextFormat::Date), None, None),
            json!("2024-02-29")
        )
        .is_ok()
    );
    assert!(
        check(
            text(Some(TextFormat::Date), None, None),
            json!("2023-02-29")
        )
        .is_err()
    );
    assert_eq!(
        check(
            text(Some(TextFormat::DateTime), None, None),
            json!("2024-03-15 14:30")
        ),
        Err("Must be a valid date-time, e.g. 2024-03-15T14:30:00Z".to_owned())
    );
    for good in [
        "2024-03-15T14:30:00Z",
        "2024-03-15T14:30Z",
        "2024-03-15T14:30:00.123+08:00",
        "2024-03-15T14:30:00+0800",
    ] {
        assert!(
            check(text(Some(TextFormat::DateTime), None, None), json!(good)).is_ok(),
            "{good}"
        );
    }
    assert_eq!(
        check(text(None, None, None), json!(3)),
        Err("Must be text".to_owned())
    );
}

/// 数字：一句话说清类型与范围；`number` 字段里整数的边界写成 `X.0`；数字的文字可以交，写成 JSON 数字。
#[test]
fn numbers_take_text_and_say_the_range() {
    assert_eq!(
        check(number(true, Some(0.0), Some(150.0)), json!("200")),
        Err("Must be an integer between 0 and 150".to_owned())
    );
    assert_eq!(
        check(number(false, Some(1.0), None), json!(0.5)),
        Err("Must be a number >= 1.0".to_owned())
    );
    assert_eq!(
        check(number(false, None, Some(2.5)), json!("abc")),
        Err("Must be a number <= 2.5".to_owned())
    );
    assert_eq!(
        check(number(true, None, None), json!("4.2")),
        Err("Must be an integer".to_owned())
    );
    assert_eq!(
        check(number(true, None, None), json!(" 42 ")),
        Ok(json!(42))
    );
    assert_eq!(
        check(number(false, None, None), json!("3.5")),
        Ok(json!(3.5))
    );
    assert_eq!(check(number(false, None, None), json!(3.0)), Ok(json!(3)));
}

/// 布尔、单选、多选。
#[test]
fn booleans_and_choices() {
    let boolean = FieldKind::Boolean { default: None };
    assert_eq!(check(boolean.clone(), json!("true")), Ok(json!(true)));
    assert_eq!(check(boolean.clone(), json!(false)), Ok(json!(false)));
    assert_eq!(
        check(boolean, json!("yes")),
        Err("Must be true or false".to_owned())
    );
    let choice = FieldKind::Choice {
        options: choices(&["a", "b"]),
        default: None,
    };
    assert_eq!(check(choice.clone(), json!("b")), Ok(json!("b")));
    assert_eq!(
        check(choice, json!("B")),
        Err(r#"Invalid option: expected one of "a"|"b""#.to_owned())
    );
    let many = FieldKind::Choices {
        options: choices(&["a", "b", "c"]),
        min_items: Some(2),
        max_items: Some(2),
        default: Vec::new(),
    };
    assert_eq!(
        check(many.clone(), json!(["a", "c"])),
        Ok(json!(["a", "c"]))
    );
    assert_eq!(
        check(many.clone(), json!(["a"])),
        Err("Select at least 2 items".to_owned())
    );
    assert_eq!(
        check(many.clone(), json!(["a", "b", "c"])),
        Err("Select at most 2 items".to_owned())
    );
    assert_eq!(
        check(many, json!(["a", "z"])),
        Err(r#"Invalid option: expected one of "a"|"b"|"c""#.to_owned())
    );
}

/// 整份回答：必填、没填（null、空文字）、未知键；结果是服务器要的类型。
#[test]
fn accept_checks_the_whole_form() {
    let fields = vec![
        field("name", true, text(None, None, None)),
        field("age", false, number(true, None, None)),
        field("ok", false, FieldKind::Boolean { default: None }),
    ];
    assert_eq!(
        accept(
            &fields,
            &object(json!({"name": "dev", "age": "7", "ok": "false"}))
        ),
        Ok(object(json!({"name": "dev", "age": 7, "ok": false})))
    );
    assert_eq!(
        accept(&fields, &object(json!({"name": "", "age": null}))),
        Err(FieldError {
            field: "name".to_owned(),
            message: "This field is required".to_owned()
        })
    );
    assert_eq!(
        accept(&fields, &object(json!({"name": "dev", "age": ""}))),
        Ok(object(json!({"name": "dev"})))
    );
    assert_eq!(
        accept(&fields, &object(json!({"name": "dev", "extra": 1}))),
        Err(FieldError {
            field: "extra".to_owned(),
            message: "Is not a field of this form".to_owned()
        })
    );
    assert_eq!(
        accept(&fields, &object(json!({"name": "dev", "age": "x"}))),
        Err(FieldError {
            field: "age".to_owned(),
            message: "Must be an integer".to_owned()
        })
    );
}

/// 初值：schema 的 `default`。
#[test]
fn initial_values_come_from_the_defaults() {
    let number = field(
        "n",
        false,
        FieldKind::Number {
            integer: true,
            minimum: None,
            maximum: None,
            default: Some(5.0),
        },
    );
    assert_eq!(number.initial(), Some(json!(5.0)));
    let many = field(
        "m",
        false,
        FieldKind::Choices {
            options: choices(&["a"]),
            min_items: None,
            max_items: None,
            default: vec!["a".to_owned()],
        },
    );
    assert_eq!(many.initial(), Some(json!(["a"])));
    assert_eq!(field("t", false, text(None, None, None)).initial(), None);
}

/// 回答的 JSON 与别的询问的回答不重名（`Answer` 是 untagged）。
#[test]
fn answers_read_back_as_their_own_kind() {
    use crate::interaction::Answer;
    for answer in [
        Answer::McpElicitation(ElicitationAnswer::Accept {
            content: object(json!({"a": 1})),
        }),
        Answer::McpElicitation(ElicitationAnswer::Decline),
        Answer::McpElicitation(ElicitationAnswer::Cancel),
        Answer::McpSampling(SamplingAnswer::SamplingAllowed { session: true }),
        Answer::McpSampling(SamplingAnswer::SamplingDenied),
    ] {
        let text = serde_json::to_string(&answer).unwrap();
        assert_eq!(
            serde_json::from_str::<Answer>(&text).unwrap(),
            answer,
            "{text}"
        );
    }
}
