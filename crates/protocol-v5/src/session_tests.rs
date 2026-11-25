//! 生成的会话类型（docs/zcode-protocol-v5/02-session-domain.md v0.4，11-contract-codegen.md §4、§5）：往返不变、宽松读端的降级、
//! 命令参数严格，以及给 rust 的 schema 带着会话命令 id 的规则。与 TS 侧 `packages/protocol/test/sessionContract.test.ts` 对着同一份 02。
use rust_cli_protocol_core::schema::Node;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::generated::session_fork_v1::CommandTurnFork;
use crate::generated::session_v1::{
    CommandInputSend, CommandResultInputSend, CommandResultInputSendDelivery, CommandTurnStop,
    InputFate, InputFateFate, QuerySessionsFactsResult, SessionState,
    SessionStateQueuePolicyPauseReason,
};

const SESSION_SCHEMA: &str = include_str!("../schema/session@1.json");
const V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0123456789ab";

/// 读出来再写回去，值不变（副本转发不丢东西）。
fn round_trip<T: DeserializeOwned + serde::Serialize>(value: Value) -> T {
    let decoded: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    decoded
}

fn session_schema(name: &str) -> Node {
    let file: Value = serde_json::from_str(SESSION_SCHEMA).unwrap();
    Node::compile(&file["schemas"][name]).unwrap_or_else(|error| panic!("{name}: {error}"))
}

#[test]
fn every_session_schema_compiles_in_the_rust_validator() {
    let file: Value = serde_json::from_str(SESSION_SCHEMA).unwrap();
    for (name, node) in file["schemas"].as_object().unwrap() {
        Node::compile(node).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
fn a_session_command_id_must_be_a_uuid_v7() {
    let id = session_schema("SessionCommandIdSchema");
    assert!(id.validate(&json!(V7), "").is_ok());
    // v4 与 SDK 旧的 `<clientId>-<序号>` 都不是（02 §11.2 A）
    assert!(
        id.validate(&json!("019a5f3c-7a2b-4c3d-8e4f-0123456789ab"), "")
            .is_err()
    );
    assert!(id.validate(&json!("window-1-1"), "").is_err());
}

#[test]
fn an_input_fate_round_trips_and_an_unknown_fate_fails_closed() {
    let fate: InputFate = round_trip(json!({
        "id": V7, "v": 7, "fate": "cancelled", "turnId": "turn_1",
        "reason": "admission.inputCancelled", "at": 1_793_624_179_370_u64
    }));
    assert_eq!(fate.fate, InputFateFate::Cancelled);
    // 新对端的值：保留原文、写回原样；按 .catch 降级成 failed（02 §1.5：当成“还没完”会让结算一直等）
    let newer: InputFate =
        serde_json::from_value(json!({"id": V7, "v": 1, "fate": "parked", "at": 1})).unwrap();
    assert_eq!(newer.fate, InputFateFate::Unrecognized("parked".to_owned()));
    assert_eq!(newer.fate.resolved(), InputFateFate::Failed);
    assert_eq!(
        serde_json::to_value(&newer).unwrap()["fate"],
        json!("parked")
    );
}

#[test]
fn session_state_defaults_carry_the_running_turn_and_the_rows_window() {
    let state: SessionState = serde_json::from_value(json!({})).unwrap();
    assert_eq!(state.control.turn_id, None);
    assert_eq!(state.rows_window.total_count, 0);
    assert_eq!(state.rows_window.first_id, None);
    let running: SessionState = serde_json::from_value(json!({
        "control": {"phase": "running", "turnId": "turn_1",
                    "activeWorks": [{"id": "turn_1", "kind": "turn", "foregroundExecutionId": "old"}]},
        "rowsWindow": {"totalCount": 500, "firstId": "msg_0"},
        "queuePolicy": {"autoDrain": false, "pauseReason": "napping"}
    }))
    .unwrap();
    assert_eq!(running.control.turn_id.as_deref(), Some("turn_1"));
    assert_eq!(running.rows_window.total_count, 500);
    let reason = running.queue_policy.pause_reason.unwrap();
    assert_eq!(
        reason.resolved(),
        SessionStateQueuePolicyPauseReason::Stopped
    );
}

#[test]
fn named_turn_commands_are_strict() {
    let stop: CommandTurnStop = round_trip(json!({"turnId": "turn_1"}));
    assert_eq!(stop.turn_id, "turn_1");
    // v0.3 的 expectedExecutionId 是未知字段；turnId 必填（02 §4.2）
    assert!(
        serde_json::from_value::<CommandTurnStop>(
            json!({"turnId": "t", "expectedExecutionId": "x"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<CommandTurnStop>(json!({})).is_err());
    let guide: CommandInputSend = serde_json::from_value(
        json!({"text": "改一下", "disposition": "guide", "turnId": "turn_1"}),
    )
    .unwrap();
    assert_eq!(guide.turn_id.as_deref(), Some("turn_1"));
    // 命令里不认识的处置拒绝，不降级（11 §5）
    assert!(
        serde_json::from_value::<CommandInputSend>(
            json!({"text": "x", "disposition": "interrupt"})
        )
        .is_err()
    );
    let fork: CommandTurnFork = round_trip(json!({"sessionId": "sess_new"}));
    assert_eq!(fork.session_id, "sess_new");
}

#[test]
fn command_results_degrade_like_events() {
    let queued: CommandResultInputSend = round_trip(json!({"delivery": "queue", "inputId": V7}));
    assert_eq!(queued.delivery, CommandResultInputSendDelivery::Queue);
    let newer: CommandResultInputSend =
        serde_json::from_value(json!({"delivery": "teleport", "inputId": V7})).unwrap();
    assert_eq!(
        newer.delivery.resolved(),
        CommandResultInputSendDelivery::Queue
    );
}

#[test]
fn a_facts_lookup_has_the_fact_or_null() {
    let result: QuerySessionsFactsResult = serde_json::from_value(json!({"sessions": [
        {"sessionId": "s1",
         "fact": {"title": "改名后", "titleSource": "custom", "createdAt": 1, "updatedAt": 5, "deletedAt": 9}},
        {"sessionId": "s2", "fact": null}
    ]}))
    .unwrap();
    // 删除了的会话照样有事实（deletedAt 有值）；库里没有的是 null（02 §8）
    let deleted = result.sessions[0].fact.as_ref().unwrap();
    assert_eq!(deleted.deleted_at, Some(9.0));
    assert_eq!(result.sessions[1].fact, None);
    // rust 是发送方：null 照样写出（11 §4「必填时照样写出 null」），Host 读到的永远是显式的 null
    assert_eq!(
        serde_json::to_value(&result.sessions[1]).unwrap(),
        json!({"sessionId": "s2", "fact": null})
    );
    let params = session_schema("Query.sessions.facts.params");
    assert!(params.validate(&json!({"sessionIds": []}), "").is_err());
    assert!(params.validate(&json!({"sessionIds": ["s1"]}), "").is_ok());
}
