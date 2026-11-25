//! 子代理与任务工具给自动模式的分类器看的内容（`Tool::classifier_input`，docs/specs/rust-cli/auto-classifier.md §15.3）。

use rust_cli_kernel_api::classifier::{ClassifierInput, js_text};
use serde_json::Value;

fn str_of<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

fn present<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    str_of(input, key).filter(|text| !text.is_empty())
}

/// `(<类型>): <prompt>`，没有类型时 `: <prompt>`。
pub(crate) fn agent(input: &Value) -> ClassifierInput {
    let Some(prompt) = str_of(input, "prompt") else {
        return ClassifierInput::raw(input);
    };
    match present(input, "subagent_type") {
        Some(kind) => ClassifierInput::text(format!("({kind}): {prompt}")),
        None => ClassifierInput::text(format!(": {prompt}")),
    }
}

/// `to <收件人>: <消息>`，带摘要与“空闲时通知”的标记。
pub(crate) fn send_message(input: &Value) -> ClassifierInput {
    let (Some(to), Some(message)) = (input.get("to"), str_of(input, "message")) else {
        return ClassifierInput::raw(input);
    };
    let to = js_text(to);
    let summary = present(input, "summary")
        .map_or_else(String::new, |summary| format!(" [summary: {summary}]"));
    if input.get("notify_when_idle") == Some(&Value::Bool(true)) {
        return ClassifierInput::text(if message.trim().is_empty() {
            format!("notify when idle: {to}{summary}")
        } else {
            format!("to {to}: {message}{summary} [notify when idle]")
        });
    }
    ClassifierInput::text(format!("to {to}: {message}{summary}"))
}

/// 任务的 ID，有状态、主题时接在后面。
pub(crate) fn task_update(input: &Value) -> ClassifierInput {
    let Some(id) = input.get("taskId").filter(|value| !value.is_null()) else {
        return ClassifierInput::raw(input);
    };
    let mut parts = vec![js_text(id)];
    parts.extend(present(input, "status").map(str::to_owned));
    parts.extend(present(input, "subject").map(str::to_owned));
    ClassifierInput::text(parts.join(" "))
}

#[cfg(test)]
#[path = "classifier_input_tests.rs"]
mod tests;
