//! 后台工作的事实与工具结果里的键（session-lifecycle.md §10.28）。
use super::*;
use crate::tool::text;

#[test]
fn a_launch_is_read_back_from_the_display_next_to_other_keys() {
    let mut output = text("Command running in background with ID: b1".to_owned());
    output.display = serde_json::json!({ "agent": { "agentId": "agent_1" } });
    let launch = BackgroundChange::Launched {
        id: "b1".to_owned(),
        kind: BackgroundKind::Shell,
        description: "build".to_owned(),
        child: None,
    };
    let output = launch.clone().attach(output);
    // 别的键不动（Agent 工具的 `agent` 键与它并列）。
    assert_eq!(output.display["agent"]["agentId"], "agent_1");
    assert_eq!(output.display[DISPLAY_KEY]["change"], "launched");
    assert!(output.display[DISPLAY_KEY].get("child").is_none());
    assert_eq!(BackgroundChange::of(&output), Some(launch));
}

#[test]
fn a_stop_is_written_into_an_empty_display() {
    let stop = BackgroundChange::Stopped {
        id: "b2".to_owned(),
    };
    let output = stop.clone().attach(text(String::new()));
    assert_eq!(BackgroundChange::of(&output), Some(stop));
}

#[test]
fn a_result_without_the_key_changes_nothing() {
    assert_eq!(BackgroundChange::of(&text("done".to_owned())), None);
}

#[test]
fn starts_round_trip_without_the_optional_fields() {
    let start = BackgroundStart {
        id: "agent_1".to_owned(),
        kind: BackgroundKind::Agent,
        description: "Find callers".to_owned(),
        tool_use: Some(ToolUseId::new("toolu_1")),
        child: Some(SessionId::new("sess_subagent_agent_1")),
        started_at: 1_700_000_000_000,
    };
    let json = serde_json::to_value(&start).unwrap();
    assert_eq!(json["kind"], "agent");
    assert_eq!(
        serde_json::from_value::<BackgroundStart>(json).unwrap(),
        start
    );
    let bare = BackgroundStart {
        tool_use: None,
        child: None,
        ..start
    };
    let json = serde_json::to_value(&bare).unwrap();
    assert!(json.get("tool_use").is_none());
    assert!(json.get("child").is_none());
}
