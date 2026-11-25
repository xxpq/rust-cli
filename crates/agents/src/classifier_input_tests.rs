//! 子代理与任务工具的投影（auto-classifier.md §15.3）。

use rust_cli_kernel_api::classifier::ClassifierInput::{Skip, Text};
use serde_json::json;

use super::*;

fn text(value: &str) -> ClassifierInput {
    Text(value.to_owned())
}

#[test]
fn agent_names_the_type_and_the_prompt() {
    assert_eq!(
        agent(&json!({"prompt": "look", "subagent_type": "Explore"})),
        text("(Explore): look")
    );
    assert_eq!(agent(&json!({"prompt": "look"})), text(": look"));
}

#[test]
fn send_message_names_the_recipient() {
    assert_eq!(
        send_message(&json!({"to": "s2", "message": "hi", "summary": "greet"})),
        text("to s2: hi [summary: greet]")
    );
    assert_eq!(
        send_message(&json!({"to": "s2", "message": "hi", "notify_when_idle": true})),
        text("to s2: hi [notify when idle]")
    );
    assert_eq!(
        send_message(&json!({"to": "s2", "message": " ", "notify_when_idle": true})),
        text("notify when idle: s2")
    );
}

#[test]
fn task_tools_project_ids_and_subjects() {
    assert_eq!(
        task_update(&json!({"taskId": "3", "status": "completed", "subject": "Fix"})),
        text("3 completed Fix")
    );
    assert_eq!(task_update(&json!({"taskId": "3"})), text("3"));
    assert_eq!(
        ClassifierInput::first_of(&json!({}), &["task_id", "shell_id"]),
        Skip
    );
}
