//! serve 的消息：握手的解析与约束、打断、错误帧、给 headless 的行加 `turn_id` 与 `seq`。

use super::*;

#[test]
fn a_session_open_carries_the_whole_turn() {
    let text = json!({
        "type": "session_open",
        "user": "u-1",
        "turn_id": "t-1",
        "session": {"resume": "6f1d4c2a-8b3e-4f5a-9c7d-2e1b0a9f8c7d"},
        "primary_env": {"relay": {"url": "wss://relay.example.com/c/1", "user": "u-1", "workdir": "/home/z/p"}},
        "exec_token": "tok",
        "provider_config": {"providers": {}},
        "model": "zgo/m",
        "effort": "high",
        "permission_mode": "bypassPermissions",
        "max_turns": 30,
        "append_system_prompt": "extra",
        "disallowed_tools": ["WebSearch"],
        "partial_messages": true,
    })
    .to_string();
    let First::SessionOpen(open) = first(&text).unwrap() else {
        panic!("expected session_open");
    };
    assert_eq!(open.user, "u-1");
    assert_eq!(
        open.session,
        Target::Resume("6f1d4c2a-8b3e-4f5a-9c7d-2e1b0a9f8c7d".to_owned())
    );
    let Some(PrimaryEnvSpec::Relay(relay)) = &open.primary_env else {
        panic!("expected a relay");
    };
    assert_eq!(relay.workdir, "/home/z/p");
    assert_eq!(relay.up_url, None);
    assert_eq!(open.max_turns, Some(30));
    assert!(open.partial_messages);
}

#[test]
fn unknown_fields_and_types_are_refused() {
    let unknown = json!({"type": "session_open", "user": "u", "turn_id": "t", "session": {"new": "x"}, "prompt": "no"});
    assert!(first(&unknown.to_string()).is_err());
    assert!(first(&json!({"type": "hello"}).to_string()).is_err());
    assert!(first("not json").is_err());
    let attach = first(
        &json!({"type": "session_attach", "user": "u", "turn_id": "t", "after_seq": 4}).to_string(),
    );
    assert_eq!(
        attach.unwrap(),
        First::SessionAttach(Attach {
            user: "u".to_owned(),
            turn_id: "t".to_owned(),
            after_seq: 4,
        })
    );
}

#[test]
fn only_an_interrupt_control_request_is_recognized() {
    let interrupt = json!({"type": "control_request", "request_id": "r-1", "request": {"subtype": "interrupt"}});
    assert_eq!(
        interrupt_request(&interrupt.to_string()),
        Some("r-1".to_owned())
    );
    let other = json!({"type": "control_request", "request_id": "r-2", "request": {"subtype": "set_model"}});
    assert_eq!(interrupt_request(&other.to_string()), None);
    assert_eq!(interrupt_request("{\"type\":\"user\"}"), None);
    assert_eq!(
        control_success("r-1"),
        "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"r-1\"}}"
    );
}

#[test]
fn tagging_keeps_the_field_order_and_appends_turn_and_seq() {
    let line = "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}";
    assert_eq!(
        tag(line, "t-1", 7),
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"turn_id\":\"t-1\",\"seq\":7}"
    );
    assert_eq!(
        tag("plain", "t-1", 1),
        "{\"type\":\"raw\",\"text\":\"plain\",\"turn_id\":\"t-1\",\"seq\":1}"
    );
    assert_eq!(
        error_frame(code::SESSION_NOT_FOUND, "gone"),
        "{\"type\":\"session_error\",\"code\":\"session_not_found\",\"message\":\"gone\"}"
    );
}
