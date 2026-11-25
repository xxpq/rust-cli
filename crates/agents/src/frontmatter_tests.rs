use super::{Value, parse, text, tools};

#[test]
fn tools_are_a_comma_list_or_a_list() {
    let map = parse("---\nname: a\ntools: Read, Grep\ndisallowedTools:\n  - Bash\n---\n").fields;
    assert_eq!(
        tools(map.get("tools")),
        Some(vec!["Read".to_owned(), "Grep".to_owned()])
    );
    assert_eq!(
        tools(map.get("disallowedTools")),
        Some(vec!["Bash".to_owned()])
    );
}

#[test]
fn star_or_missing_tools_mean_all_tools() {
    let map = parse("---\nname: a\ntools: [Read, \"*\"]\n---\n").fields;
    assert_eq!(tools(map.get("tools")), None);
    assert_eq!(tools(None), None);
}

/// 嵌套的对象与空着的键：标量字段按没写；工具清单写了却不是字符串或列表时是空清单（不是一个怪名字，也不是全部工具；
/// agents.md §3）。
#[test]
fn a_nested_object_or_an_empty_key_is_not_text() {
    let map =
        parse("---\nname: a\ntools:\n  Read: true\ndescription:\n  nested: text\nmodel:\n---\n")
            .fields;
    assert!(matches!(map.get("tools"), Some(Value::Map(_))));
    assert_eq!(tools(map.get("tools")), Some(Vec::new()));
    assert_eq!(text(&map, "description"), None);
    assert_eq!(text(&map, "model"), None);
}

/// 下一行缩进的普通文字是这个键的文本（YAML 的多行标量）。LIB-SMALL 以前自写的解析器把它当嵌套块、按没写。
#[test]
fn a_plain_scalar_on_the_next_line_is_text() {
    let map = parse("---\nname: a\ndescription:\n  nested text\n---\n").fields;
    assert_eq!(text(&map, "description").as_deref(), Some("nested text"));
}

/// 数与布尔按 JS 的 `String(v)` 读成文字。
#[test]
fn numbers_and_booleans_read_as_text() {
    let map = parse("---\nname: a\nmaxTurns: 5\nbackground: true\n---\n").fields;
    assert_eq!(text(&map, "maxTurns").as_deref(), Some("5"));
    assert_eq!(text(&map, "background").as_deref(), Some("true"));
}
