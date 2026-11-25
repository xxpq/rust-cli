//! 工具给自动模式的分类器看的内容（`Tool::classifier_input`，docs/specs/rust-cli/auto-classifier.md §15.3）。
//!
//! 写法照对照实现各工具的投影：字符串字段原样，几个字段时按它的模板拼接；输入缺了要的字段时用原始输入的 JSON 文本。

use rust_cli_base::js::string::utf16_prefix;
use rust_cli_kernel_api::classifier::{ClassifierInput, js_text};
use rust_cli_kernel_api::tool_names;
use serde_json::{Map, Value, json};

/// Edit 的投影里被删掉的原文最多保留这么多（UTF-16 单元，对照实现的 `editRemovalCap`）。
const EDIT_REMOVAL_CAP: usize = 3_000;

/// AskUserQuestion 的投影里问题与选项各最多写这么多条，多出的写一句省略。
const QUESTION_ENTRIES_CAP: usize = 256;

/// AskUserQuestion 的输入形状不对时的投影。
const MALFORMED_QUESTIONS: &str = "[malformed AskUserQuestion input]";

fn str_of<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

/// `<a>: <b>`；任一不是字符串时是原始输入。
fn pair(input: &Value, first: &str, second: &str) -> ClassifierInput {
    match (str_of(input, first), str_of(input, second)) {
        (Some(a), Some(b)) => ClassifierInput::text(format!("{a}: {b}")),
        _ => ClassifierInput::raw(input),
    }
}

/// `<a>: <b>`，`b` 为空或没有时只有 `a`（WebFetch 的 `url` 与 `prompt`）。
fn with_optional(input: &Value, first: &str, second: &str, glue: &str) -> ClassifierInput {
    let Some(a) = str_of(input, first) else {
        return ClassifierInput::raw(input);
    };
    match str_of(input, second).filter(|b| !b.is_empty()) {
        Some(b) => ClassifierInput::text(format!("{a}{glue}{b}")),
        None => ClassifierInput::text(a.to_owned()),
    }
}

pub(crate) fn write(input: &Value) -> ClassifierInput {
    pair(input, "file_path", "content")
}

pub(crate) fn web_fetch(input: &Value) -> ClassifierInput {
    with_optional(input, "url", "prompt", ": ")
}

pub(crate) fn grep(input: &Value) -> ClassifierInput {
    with_optional(input, "pattern", "path", " in ")
}

pub(crate) fn cron_create(input: &Value) -> ClassifierInput {
    pair(input, "cron", "prompt")
}

/// 新旧两段都给分类器（删掉的原文过长时截断并标出），`replace_all` 只在为真时写。
pub(crate) fn edit(input: &Value) -> ClassifierInput {
    let (Some(path), Some(old), Some(new)) = (
        str_of(input, "file_path"),
        str_of(input, "old_string"),
        str_of(input, "new_string"),
    ) else {
        return ClassifierInput::raw(input);
    };
    let removes = utf16_prefix(old, EDIT_REMOVAL_CAP);
    let mut object = Map::new();
    object.insert("file_path".into(), json!(path));
    object.insert("adds".into(), json!(new));
    object.insert("removes".into(), json!(removes));
    object.insert("removesTruncated".into(), json!(removes.len() < old.len()));
    if input.get("replace_all") == Some(&Value::Bool(true)) {
        object.insert("replaceAll".into(), json!(true));
    }
    ClassifierInput::Json(Value::Object(object))
}

/// 单元格的位置、模式与新内容；删除时新内容不生效，写成 `ignored_source`。
pub(crate) fn notebook_edit(input: &Value) -> ClassifierInput {
    let (Some(path), Some(source)) = (
        str_of(input, "notebook_path"),
        input.get("new_source").filter(|value| !value.is_null()),
    ) else {
        return ClassifierInput::raw(input);
    };
    let mode = str_of(input, "edit_mode").unwrap_or("replace");
    let deleting = mode == "delete";
    let mut object = Map::new();
    object.insert("notebook_path".into(), json!(path));
    object.insert("mode".into(), json!(mode));
    if let Some(cell) = input.get("cell_id").filter(|value| !value.is_null()) {
        object.insert("cell_id".into(), cell.clone());
    }
    if !deleting && let Some(kind) = input.get("cell_type").filter(|value| !value.is_null()) {
        object.insert("cell_type".into(), kind.clone());
    }
    let key = if deleting { "ignored_source" } else { "adds" };
    object.insert(key.into(), source.clone());
    ClassifierInput::Json(Value::Object(object))
}

pub(crate) fn schedule_wakeup(input: &Value) -> ClassifierInput {
    if input.get("stop") == Some(&Value::Bool(true)) {
        return ClassifierInput::text(
            "stop the /loop \u{2014} cancel pending wakeups, schedule nothing".to_owned(),
        );
    }
    let present = |key| input.get(key).filter(|value| !value.is_null());
    match (present("delaySeconds"), present("prompt")) {
        (Some(delay), Some(prompt)) => {
            ClassifierInput::text(format!("wake in {}s: {}", js_text(delay), js_text(prompt)))
        }
        _ => ClassifierInput::text(format!(
            "malformed {} call missing delaySeconds/prompt \u{2014} the tool will reject it",
            tool_names::SCHEDULE_WAKEUP
        )),
    }
}

/// 命令，或 WebSocket 的地址（带子协议时列出）。
pub(crate) fn monitor(input: &Value) -> ClassifierInput {
    let Some(ws) = input.get("ws").filter(|value| value.is_object()) else {
        return ClassifierInput::first_of(input, &["command"]);
    };
    let url = ws.get("url").map(js_text).unwrap_or_default();
    let protocols: Vec<String> = ws
        .get("protocols")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| format!("\"{}\"", js_text(item)))
                .collect()
        })
        .unwrap_or_default();
    let subprotocols = if protocols.is_empty() {
        String::new()
    } else {
        format!(" (subprotocols: {})", protocols.join(", "))
    };
    ClassifierInput::text(format!("websocket {url}{subprotocols}"))
}

pub(crate) fn report_findings(input: &Value) -> ClassifierInput {
    match input.get("findings").and_then(Value::as_array) {
        Some(findings) => ClassifierInput::text(format!("{} findings", findings.len())),
        None => ClassifierInput::raw(input),
    }
}

/// 问题、表头与选项写成一行：把连续空白换成一个空格、去首尾空白、转义 `\` 与 `"`（不是字符串的写空）。
fn question_text(value: Option<&Value>) -> String {
    let Some(text) = value.and_then(Value::as_str) else {
        return String::new();
    };
    let collapsed: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == '\u{85}')
        .filter(|part| !part.is_empty())
        .collect();
    collapsed
        .join(" ")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn options_text(options: Option<&Value>) -> String {
    let Some(options) = options.and_then(Value::as_array) else {
        return String::new();
    };
    let mut items = Vec::new();
    for (index, option) in options.iter().enumerate() {
        if items.len() >= QUESTION_ENTRIES_CAP {
            items.push("[\u{2026} further option entries elided]".to_owned());
            break;
        }
        let number = index + 1;
        if !option.is_object() {
            items.push(format!("({number}) \"{}\"", question_text(Some(option))));
            continue;
        }
        let mut parts = vec![format!(
            "({number}) \"{}\"",
            question_text(option.get("label"))
        )];
        let description = question_text(option.get("description"));
        if !description.is_empty() {
            parts.push(format!("\u{2014} {description}"));
        }
        let preview = question_text(option.get("preview"));
        if !preview.is_empty() {
            parts.push(format!("[preview: {preview}]"));
        }
        items.push(parts.join(" "));
    }
    items.join(" ")
}

/// 每个问题一段：`"<问题>" [header: …] [multiSelect] options: (1) "<label>" — <description> …`，问题之间用 ` | `。
pub(crate) fn ask_user_question(input: &Value) -> ClassifierInput {
    let Some(questions) = input.get("questions").and_then(Value::as_array) else {
        return ClassifierInput::text(MALFORMED_QUESTIONS.to_owned());
    };
    let mut entries = Vec::new();
    for question in questions {
        if entries.len() >= QUESTION_ENTRIES_CAP {
            entries.push(format!(
                "[\u{2026} further question entries elided: reported length {}]",
                questions.len()
            ));
            break;
        }
        if !question.is_object() {
            let text = question_text(Some(question));
            entries.push(if text.is_empty() {
                "[malformed question entry]".to_owned()
            } else {
                format!("[malformed question entry] {text}")
            });
            continue;
        }
        let header = question_text(question.get("header"));
        let header = if header.is_empty() {
            String::new()
        } else {
            format!(" [header: {header}]")
        };
        let multi = if question.get("multiSelect") == Some(&Value::Bool(true)) {
            " [multiSelect]"
        } else {
            ""
        };
        entries.push(format!(
            "\"{}\"{header}{multi} options: {}",
            question_text(question.get("question")),
            options_text(question.get("options"))
        ));
    }
    if entries.is_empty() {
        return ClassifierInput::text(MALFORMED_QUESTIONS.to_owned());
    }
    ClassifierInput::text(entries.join(" | "))
}

#[cfg(test)]
#[path = "classifier_input_tests.rs"]
mod tests;
