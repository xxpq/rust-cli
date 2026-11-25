//! 工具的投影（docs/specs/rust-cli/auto-classifier.md §15.3）：每个工具一条，写法照对照实现。

use rust_cli_kernel_api::classifier::ClassifierInput::{self, Json, Skip, Text};
use serde_json::json;

use super::*;

fn text(value: &str) -> ClassifierInput {
    Text(value.to_owned())
}

#[test]
fn file_tools_project_paths_and_contents() {
    assert_eq!(
        write(&json!({"file_path": "/w/a.txt", "content": "hi"})),
        text("/w/a.txt: hi")
    );
    assert_eq!(
        write(&json!({"file_path": "/w/a.txt"})),
        text(r#"{"file_path":"/w/a.txt"}"#)
    );
    assert_eq!(
        grep(&json!({"pattern": "TODO", "path": "src"})),
        text("TODO in src")
    );
    assert_eq!(grep(&json!({"pattern": "TODO", "path": ""})), text("TODO"));
    assert_eq!(
        web_fetch(&json!({"url": "https://example.com", "prompt": "sum"})),
        text("https://example.com: sum")
    );
    assert_eq!(
        web_fetch(&json!({"url": "https://example.com"})),
        text("https://example.com")
    );
    assert_eq!(
        cron_create(&json!({"cron": "*/5 * * * *", "prompt": "check"})),
        text("*/5 * * * *: check")
    );
}

/// Edit 带删掉的原文，超过 3000 个 UTF-16 单元截断；`replaceAll` 只在为真时写。键序同对照实现。
#[test]
fn edit_shows_what_it_adds_and_removes() {
    let short = edit(&json!({"file_path": "/w/a", "old_string": "x", "new_string": "y"}));
    assert_eq!(
        short,
        Json(json!({"file_path": "/w/a", "adds": "y", "removes": "x", "removesTruncated": false}))
    );
    let Json(value) = &short else { panic!() };
    assert_eq!(
        rust_cli_base::js::json::stringify(value),
        r#"{"file_path":"/w/a","adds":"y","removes":"x","removesTruncated":false}"#
    );
    let long = "a".repeat(3_001);
    let truncated = edit(
        &json!({"file_path": "/w/a", "old_string": long, "new_string": "", "replace_all": true}),
    );
    let Json(value) = truncated else { panic!() };
    assert_eq!(value["removes"].as_str().unwrap().len(), 3_000);
    assert_eq!(value["removesTruncated"], json!(true));
    assert_eq!(value["replaceAll"], json!(true));
}

#[test]
fn notebook_edit_names_the_cell_and_the_mode() {
    assert_eq!(
        notebook_edit(
            &json!({"notebook_path": "/w/n.ipynb", "new_source": "print(1)", "cell_id": "c1", "cell_type": "code"})
        ),
        Json(
            json!({"notebook_path": "/w/n.ipynb", "mode": "replace", "cell_id": "c1", "cell_type": "code", "adds": "print(1)"})
        )
    );
    assert_eq!(
        notebook_edit(
            &json!({"notebook_path": "/w/n.ipynb", "new_source": "", "edit_mode": "delete", "cell_type": "code"})
        ),
        Json(json!({"notebook_path": "/w/n.ipynb", "mode": "delete", "ignored_source": ""}))
    );
}

#[test]
fn schedule_wakeup_monitor_and_findings() {
    assert_eq!(
        schedule_wakeup(&json!({"delaySeconds": 300, "prompt": "go on"})),
        text("wake in 300s: go on")
    );
    assert_eq!(
        schedule_wakeup(&json!({"stop": true})),
        text("stop the /loop \u{2014} cancel pending wakeups, schedule nothing")
    );
    assert_eq!(
        schedule_wakeup(&json!({"prompt": "go on"})),
        text(
            "malformed ScheduleWakeup call missing delaySeconds/prompt \u{2014} the tool will reject it"
        )
    );
    assert_eq!(
        monitor(&json!({"command": "tail -f log", "description": "d"})),
        text("tail -f log")
    );
    assert_eq!(
        monitor(&json!({"ws": {"url": "wss://example.com/s", "protocols": ["a", "b"]}})),
        text("websocket wss://example.com/s (subprotocols: \"a\", \"b\")")
    );
    assert_eq!(
        monitor(&json!({"ws": {"url": "wss://example.com/s"}})),
        text("websocket wss://example.com/s")
    );
    assert_eq!(monitor(&json!({"description": "d"})), Skip);
    assert_eq!(
        report_findings(&json!({"findings": [{}, {}]})),
        text("2 findings")
    );
}

/// AskUserQuestion：问题、表头与选项写成一行，字段里的空白收成一个空格、引号转义。
#[test]
fn ask_user_question_summarizes_the_questions() {
    let input = json!({"questions": [
        {"question": "Which  one?", "header": "Pick", "multiSelect": true,
         "options": [{"label": "A", "description": "first \"one\""}, {"label": "B"}]},
        {"question": "Sure?", "options": []},
    ]});
    assert_eq!(
        ask_user_question(&input),
        text(
            "\"Which one?\" [header: Pick] [multiSelect] options: (1) \"A\" \u{2014} first \\\"one\\\" (2) \"B\" | \"Sure?\" options: "
        )
    );
    assert_eq!(
        ask_user_question(&json!({})),
        text("[malformed AskUserQuestion input]")
    );
    assert_eq!(
        ask_user_question(&json!({"questions": ["  x  ", 3]})),
        text("[malformed question entry] x | [malformed question entry]")
    );
}
