use super::{Frontmatter, SPACE, Value, parse, paths, tool_list};
use crate::js;

fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

fn fields_of(raw: &str) -> std::collections::BTreeMap<String, Value> {
    let parsed = parse(raw);
    assert_eq!(parsed.error, None, "{raw:?}");
    parsed.fields
}

#[test]
fn scalars_lists_and_block_text_keep_their_yaml_types() {
    let Frontmatter {
        fields,
        body,
        error,
    } = parse(
        "---\nname: reviewer\ndescription: \"Reviews code.\"\ntools: Read, Grep\ndisallowedTools:\n  - Bash\n  - 'Write'\nmaxTurns: 5\nbackground: true\nnotes: |\n  line one\n  line two\nfolded: >\n  one\n  two\nlist: [a, \"b\", ]\n---\n\nYou review code.\n",
    );
    assert_eq!(error, None);
    assert_eq!(fields["name"], text("reviewer"));
    assert_eq!(fields["description"], text("Reviews code."));
    assert_eq!(fields["tools"], text("Read, Grep"));
    assert_eq!(
        fields["disallowedTools"],
        Value::List(vec![text("Bash"), text("Write")])
    );
    assert_eq!(fields["maxTurns"], Value::Number(5.0));
    assert_eq!(fields["background"], Value::Bool(true));
    // 块文本照 YAML：`|` 留换行，`>` 折成空格，结尾留一个换行。
    assert_eq!(fields["notes"], text("line one\nline two\n"));
    assert_eq!(fields["folded"], text("one two\n"));
    assert_eq!(fields["list"], Value::List(vec![text("a"), text("b")]));
    // 结束的 `---` 之后的空白（含空行）都不算正文（`---\s*\n?`）。
    assert_eq!(body, "You review code.\n");
}

/// 嵌套的对象是 `Map`；键后面空着是 `null`；注释不是键；下一行缩进的普通文字是这个键的文本（YAML 的多行标量）。
#[test]
fn nested_objects_nulls_and_multi_line_scalars() {
    let fields = fields_of(
        "---\nhooks:\n  PreToolUse:\n    matcher: Bash   \nempty:\n\n# comment: no\nname: x\ndescription:\n  nested text\n---\n",
    );
    let Value::Map(hooks) = &fields["hooks"] else {
        panic!("hooks is a map: {:?}", fields["hooks"]);
    };
    assert!(matches!(&hooks["PreToolUse"], Value::Map(inner) if inner["matcher"] == text("Bash")));
    assert_eq!(fields["empty"], Value::Null);
    assert!(!fields.contains_key("# comment"));
    assert_eq!(fields["name"], text("x"));
    assert_eq!(fields["description"], text("nested text"));
}

/// YAML 解析失败时给含特殊字符的值加引号再解析一次（glob、`: `、`#`、`@` 等）；块列表项没法这样救，整个为空。
#[test]
fn values_with_special_characters_are_quoted_on_retry() {
    let fields = fields_of(
        "---\nname: ok\npaths: **/*.rs\nallowed-tools: Bash(git status:*)\ndescription: Note: braces {a,b}\n---\n",
    );
    assert_eq!(fields["paths"], text("**/*.rs"));
    assert_eq!(fields["allowed-tools"], text("Bash(git status:*)"));
    assert_eq!(fields["description"], text("Note: braces {a,b}"));
    // 已经是 YAML 列表的值不加引号；`[*.ts]` 不是合法的列表，整个当文字。
    assert_eq!(
        fields_of("---\npaths: [a, b]\nx: **\n---\n")["paths"],
        Value::List(vec![text("a"), text("b")])
    );
    assert_eq!(
        fields_of("---\npaths: [*.ts]\n---\n")["paths"],
        text("[*.ts]")
    );
    let broken = parse("---\npaths:\n  - **/*.ts\n---\nbody\n");
    assert!(broken.fields.is_empty());
    assert!(broken.error.is_some());
    assert_eq!(broken.body, "body\n");
}

/// 重试时行首的 tab 换成两个空格；CRLF 的行不加引号（参照的正则里 `.` 不含 `\r`），所以 CRLF 文件里没有引号的 glob 解析失败。
#[test]
fn retry_replaces_leading_tabs_but_skips_crlf_lines() {
    let fields = fields_of("---\npaths: **/*.go\nhooks:\n\tStop: y\n---\n");
    assert_eq!(fields["paths"], text("**/*.go"));
    assert!(matches!(&fields["hooks"], Value::Map(hooks) if hooks["Stop"] == text("y")));
    assert!(parse("---\r\npaths: **/*.ts\r\n---\r\n").error.is_some());
}

/// 重复的键取后一个；`<<` 合并；别名展开；非字符串的键换成 JS 的字符串。
#[test]
fn duplicate_keys_merges_aliases_and_key_strings() {
    let fields = fields_of(
        "---\nname: a\nname: b\nbase: &b {x: 1}\nchild:\n  <<: *b\n  y: 2\ncopy: *b\n1: one\ntrue: yes\n---\n",
    );
    assert_eq!(fields["name"], text("b"));
    let Value::Map(child) = &fields["child"] else {
        panic!("child is a map");
    };
    assert_eq!(child["x"], Value::Number(1.0));
    assert_eq!(child["y"], Value::Number(2.0));
    assert_eq!(fields["copy"], fields["base"]);
    assert_eq!(fields["1"], text("one"));
    // YAML 1.2：`yes` 是字符串。
    assert_eq!(fields["true"], text("yes"));
}

#[test]
fn files_without_a_closed_frontmatter_keep_the_raw_text_as_body() {
    for raw in [
        "# Just a note\n",
        "---\nname: unterminated\n",
        "\n---\nname: x\n---\nbody",
        "\u{feff}# Title\n",
    ] {
        let parsed = parse(raw);
        assert!(parsed.fields.is_empty(), "{raw:?}");
        assert_eq!(parsed.body, raw);
        assert_eq!(parsed.error, None);
    }
    let parsed = parse("\u{feff}---\r\nname: x\r\n---\r\nbody");
    assert_eq!(parsed.fields["name"], text("x"));
    assert_eq!(parsed.body, "body");
    // 结尾的 `---` 没有换行也算；值里的 `---` 也结束这一块（参照的正则）。
    let parsed = parse("---\nname: y\n---");
    assert_eq!(parsed.fields["name"], text("y"));
    assert_eq!(parsed.body, "");
    let parsed = parse("---\nname: x\ndescription: a---b\n---\nbody\n");
    assert_eq!(parsed.fields["description"], text("a"));
    assert_eq!(parsed.body, "b\n---\nbody\n");
}

/// 顶层不是对象（列表、标量、只有注释）时没有字段，也不算失败。
#[test]
fn a_document_that_is_not_an_object_has_no_fields() {
    for raw in [
        "---\n- a\n- b\n---\n",
        "---\njust text\n---\n",
        "---\n# x\n---\n",
        "---\n---\n",
    ] {
        let parsed = parse(raw);
        assert!(parsed.fields.is_empty(), "{raw:?}");
        assert_eq!(parsed.error, None, "{raw:?}");
    }
}

#[test]
fn text_and_js_string_follow_javascript() {
    assert_eq!(text("a").text().as_deref(), Some("a"));
    assert_eq!(Value::Number(5.0).text().as_deref(), Some("5"));
    assert_eq!(Value::Number(1.5).text().as_deref(), Some("1.5"));
    assert_eq!(Value::Number(1e21).js_string(), "1e+21");
    assert_eq!(Value::Number(f64::INFINITY).js_string(), "Infinity");
    assert_eq!(Value::Number(f64::NEG_INFINITY).js_string(), "-Infinity");
    assert_eq!(Value::Number(f64::NAN).js_string(), "NaN");
    assert_eq!(Value::Bool(false).text().as_deref(), Some("false"));
    assert_eq!(Value::Null.text(), None);
    assert_eq!(Value::List(vec![text("a")]).text(), None);
    assert_eq!(
        Value::List(vec![text("a"), Value::Null, Value::Number(2.0)]).js_string(),
        "a,,2"
    );
    assert_eq!(
        Value::Map(Default::default()).js_string(),
        "[object Object]"
    );
    assert_eq!(Value::Number(1.0).as_str(), None);
}

/// 正则里的空白类与 JS 的 `\s`（[`js::string::is_space`]）是同一组字符。
#[test]
fn the_space_class_is_the_javascript_one() {
    let class = regex::Regex::new(&format!("^[{SPACE}]$")).unwrap();
    for code in 0..=0x3100_u32 {
        let Some(c) = char::from_u32(code) else {
            continue;
        };
        assert_eq!(
            class.is_match(&c.to_string()),
            js::string::is_space(c),
            "{code:#x}"
        );
    }
    assert!(class.is_match("\u{feff}"));
}

/// 冒号后紧跟 tab 当作空格（ext.md §2.2 第 2 步）：顶层、嵌套、多个 tab、引号里的键都读得出；同一个文件里块文本的 tab 原样。
/// saphyr 照 YAML 测试集拒绝这种写法，以前整个 frontmatter 为空。
#[test]
fn a_tab_after_the_colon_reads_like_a_space() {
    let fields = fields_of(
        "---\nname:\tx\nhooks:\n  Stop:\t-y\nmodel:\t\tz\n\"q\":\tw\nnotes: |\n  key:\tvalue\n---\nbody",
    );
    assert_eq!(fields["name"], text("x"));
    assert_eq!(
        fields["hooks"],
        Value::Map([("Stop".to_owned(), text("-y"))].into())
    );
    assert_eq!(fields["model"], text("z"));
    assert_eq!(fields["q"], text("w"));
    assert_eq!(fields["notes"], text("key:\tvalue\n"));
    // 加引号重试的那一遍同样照这一条读：`description` 要加引号，`name:<tab>x` 不加。
    let fields = fields_of("---\ndescription: a: b\nname:\tx\n---\n");
    assert_eq!(fields["description"], text("a: b"));
    assert_eq!(fields["name"], text("x"));
    // 别的错照旧：没有冒号与 tab 的位置不改写。
    assert!(
        parse("---\nname: x\n  bad: indentation: here\n---\n")
            .error
            .is_some()
    );
}

/// 真假的写法（ext.md §2.2 的字段表）：布尔原样；字符串与数按 `String(v)` 去首尾空白、不分大小写。
#[test]
fn flags_read_the_reference_words() {
    for (value, expected) in [
        (Value::Bool(true), Some(true)),
        (Value::Bool(false), Some(false)),
        (text(" Yes "), Some(true)),
        (text("On"), Some(true)),
        (text("TRUE"), Some(true)),
        (Value::Number(1.0), Some(true)),
        (Value::Number(0.0), Some(false)),
        (text("off"), Some(false)),
        (text("No"), Some(false)),
        (text("maybe"), None),
        (text(""), None),
        (Value::Number(2.0), None),
        (Value::Null, None),
        (Value::List(vec![text("true")]), None),
    ] {
        assert_eq!(value.flag(), expected, "{value:?}");
    }
}

/// 工具清单（ext.md §2.2、agents.md §3）：括号外的逗号与空格切分，别的空白留在项里；只取字符串项；含 `*` 是 `["*"]`。
#[test]
fn tool_lists_split_like_the_reference() {
    let names = |value: Value| tool_list(&value);
    assert_eq!(names(text("Read Grep")), ["Read", "Grep"]);
    assert_eq!(
        names(text("Bash(git status:*), Read,Edit")),
        ["Bash(git status:*)", "Read", "Edit"]
    );
    assert_eq!(names(text("Read\tGrep")), ["Read\tGrep"]);
    assert_eq!(
        names(Value::List(vec![
            text("Read, Grep"),
            Value::Number(5.0),
            text("")
        ])),
        ["Read", "Grep"]
    );
    assert_eq!(names(text("Read, *")), ["*"]);
    for empty in [text(""), Value::Null, Value::Bool(true), Value::Number(3.0)] {
        assert!(names(empty.clone()).is_empty(), "{empty:?}");
    }
}

/// `paths`（ext.md §2.2、context.md §11.4）：花括号外的逗号切分、展开花括号（共用预算）、列表逐项、去掉结尾的 `/**`。
#[test]
fn paths_expand_braces_like_the_reference() {
    let globs = |value: Value| paths(Some(&value));
    assert_eq!(
        globs(text("src/*.{ts,tsx}, docs/**")),
        Some(vec![
            "src/*.ts".to_owned(),
            "src/*.tsx".to_owned(),
            "docs".to_owned()
        ])
    );
    assert_eq!(
        globs(Value::List(vec![
            text("{a,b}/{c, d}"),
            Value::List(vec![text("x{}y")]),
            Value::Number(1.0),
        ])),
        Some(
            ["a/c", "a/d", "b/c", "b/d", "x{}y"]
                .map(str::to_owned)
                .to_vec()
        )
    );
    // `}` 之后有换行时整项不展开。
    assert_eq!(globs(text("a{b,c}\nd")), Some(vec!["a{b,c}\nd".to_owned()]));
    assert_eq!(globs(text("**, /**")), None);
    assert_eq!(globs(text("")), None);
    assert_eq!(paths(None), None);
    // 预算：2^10 = 1024 项超过 1000，这一项原样留下。
    let wide = "{a,b}".repeat(10);
    assert_eq!(globs(text(&wide)), Some(vec![wide.clone()]));
}
