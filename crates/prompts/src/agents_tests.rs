//! 子代理文本：文件都在、模板变量齐全、通知的包装同形。

use super::{ALL, notification_text};
use crate::texts::render;

#[test]
fn every_agent_text_is_present() {
    for (file, text) in ALL {
        assert!(!text().is_empty(), "prompts/agents/{file}.txt is empty");
        assert!(
            !text().ends_with('\n') || *file == "notification_preamble",
            "{file}"
        );
    }
}

#[test]
fn the_notification_has_the_expected_frame() {
    let text = notification_text(&[
        ("id", "agent_1"),
        ("tool_use", "toolu_1"),
        ("status", "completed"),
        ("summary", "Agent \"Find files\" finished"),
        ("note", super::notification_note()),
        (
            "body",
            "\n<usage><subagent_tokens>13</subagent_tokens><tool_uses>0</tool_uses><duration_ms>5</duration_ms></usage>",
        ),
    ]);
    assert!(text.starts_with("<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\n"));
    assert!(text.contains("consent.\n\n<task-notification>\n<task-id>agent_1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>completed</status>\n"));
    assert!(text.ends_with(
        "<duration_ms>5</duration_ms></usage>\n</task-notification>\n</system-reminder>"
    ));
    assert!(!text.contains("{{"), "{text}");
}

#[test]
fn the_trailer_takes_the_agent_id_twice() {
    let text = render(
        super::handback_trailer(),
        &[
            ("id", "agent_9"),
            ("tokens", "1"),
            ("tool_uses", "2"),
            ("duration", "3"),
        ],
    );
    assert_eq!(
        text,
        "agentId: agent_9 (use SendMessage with to: 'agent_9', summary: '<5-10 word recap>' to continue this agent)\n<usage>subagent_tokens: 1\ntool_uses: 2\nduration_ms: 3</usage>"
    );
}

#[test]
fn several_notifications_share_one_preamble() {
    let text = super::notification_wrap(
        "<task-notification>a</task-notification>\n\n<task-notification>b</task-notification>",
    );
    assert_eq!(text.matches("[SYSTEM NOTIFICATION").count(), 1);
    assert!(text.starts_with("<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\n"));
    assert!(text.ends_with("</task-notification>\n</system-reminder>"));
}

/// 缺省前台的写法与 `Agent.json` 只差两处（agents.md §8.2.4）：描述末尾一条、`run_in_background` 的说明；schema 的其余部分
/// 与键序相同。`Agent.json` 重新生成之后这里会提醒同步改它。
#[test]
fn the_foreground_text_differs_from_the_agent_definition_in_two_places() {
    let agent = crate::definition("Agent");
    let foreground = super::foreground_tool().unwrap();
    let (kept, note) = foreground.description.rsplit_once("\n- ").unwrap();
    let (agent_kept, agent_note) = agent.description.rsplit_once("\n- ").unwrap();
    assert_eq!(kept, agent_kept);
    assert!(
        agent_note.starts_with("Subagents run in the background by default;"),
        "{agent_note}"
    );
    assert!(
        note.starts_with(
            "A subagent runs in the background only if you pass `run_in_background: true`. When one does run in the background, you'll be notified when it completes."
        ),
        "{note}"
    );
    let shared = "Never fabricate or predict a pending agent's results — the notification is never something you write yourself; if the user asks before it arrives, say it's still running.";
    assert!(note.ends_with(shared) && agent_note.ends_with(shared));
    let parameter = &foreground.input_schema["properties"]["run_in_background"]["description"];
    assert_eq!(
        parameter,
        "Set to true to run this agent in the background. You will be notified when it completes."
    );
    let mut expected = agent.schema.clone();
    expected["properties"]["run_in_background"]["description"] = parameter.clone();
    assert_eq!(foreground.input_schema, expected);
    let keys = |schema: &serde_json::Value| -> Vec<String> {
        schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(keys(&foreground.input_schema), keys(&agent.schema));
}
