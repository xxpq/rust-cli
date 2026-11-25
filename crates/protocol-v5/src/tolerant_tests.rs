use serde::Deserialize;
use serde_json::json;

use super::*;
use crate::generated::core_v1::{
    InitializeParams, InitializeParamsPeerKind, InitializeResult, InitializeResultPrincipal,
    ProtocolError, ProtocolErrorOutcome,
};

#[test]
fn unknown_fields_are_ignored_and_defaults_fill_in() {
    let params: InitializeParams = serde_json::from_value(json!({
        "clientId": "c", "caps": {"core": [1]}, "later": {"x": 1},
        "peer": {"name": "n", "color": "blue"}
    }))
    .unwrap();
    assert_eq!(params.require, Vec::<String>::new());
    assert_eq!(params.peer.kind, InitializeParamsPeerKind::Unknown);
    assert_eq!(params.peer.build.version, "");
    // 缺了整个 peer：取 zod 的缺省对象
    let bare: InitializeParams =
        serde_json::from_value(json!({"clientId": "c", "caps": {}})).unwrap();
    assert_eq!(bare.peer.name, "");
}

#[test]
fn the_envelope_stays_strict() {
    assert!(serde_json::from_value::<InitializeParams>(json!({"caps": {}})).is_err());
    assert!(
        serde_json::from_value::<InitializeParams>(json!({"clientId": "c", "caps": {"core": "x"}}))
            .is_err()
    );
}

#[test]
fn unknown_enum_values_keep_the_text_and_resolve_to_the_fallback() {
    let params: InitializeParams = serde_json::from_value(
        json!({"clientId": "c", "caps": {}, "peer": {"kind": "space-probe"}}),
    )
    .unwrap();
    assert_eq!(
        params.peer.kind,
        InitializeParamsPeerKind::Unrecognized("space-probe".to_owned())
    );
    assert_eq!(
        params.peer.kind.resolved(),
        InitializeParamsPeerKind::Unknown
    );
    // 原样写回（副本转发不丢值）
    assert_eq!(
        serde_json::to_value(&params.peer.kind).unwrap(),
        json!("space-probe")
    );
    // 不是字符串也降级
    let other: InitializeParams =
        serde_json::from_value(json!({"clientId": "c", "caps": {}, "peer": {"kind": 7}})).unwrap();
    assert_eq!(
        other.peer.kind.resolved(),
        InitializeParamsPeerKind::Unknown
    );
    // 必填 + catch：缺了取降级值
    let result: InitializeResult = serde_json::from_value(json!({
        "connectionId": "x", "delivery": "continuous", "caps": {},
        "limits": {"maxMessageBytes": 1, "initialCredit": 1}, "contract": "c"
    }))
    .unwrap();
    assert_eq!(result.principal, InitializeResultPrincipal::Unknown);
}

#[test]
fn a_caught_field_reads_its_fallback_and_an_outer_optional_stays_absent() {
    let error: ProtocolError =
        serde_json::from_value(json!({"code": "x.y", "message": 5, "retryable": "no"})).unwrap();
    assert_eq!(error.message, "");
    assert!(!error.retryable);
    assert_eq!(error.outcome, None);
    let error: ProtocolError = serde_json::from_value(
        json!({"code": "x.y", "message": "m", "retryable": true, "outcome": "later"}),
    )
    .unwrap();
    assert_eq!(
        error.outcome.map(|o| o.resolved()),
        Some(ProtocolErrorOutcome::Unknown)
    );
    // 写出时没有的字段不写
    let error: ProtocolError =
        serde_json::from_value(json!({"code": "x.y", "message": "m", "retryable": true})).unwrap();
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({"code": "x.y", "message": "m", "retryable": true})
    );
}

#[derive(Debug, Deserialize, PartialEq)]
struct Item {
    id: String,
    v: u64,
    name: String,
}

#[test]
fn one_bad_entity_becomes_a_placeholder() {
    let items = decode_collection::<Item>(&json!([
        {"id": "a", "v": 1, "name": "x"},
        {"id": "b", "v": 2},
        {"name": 3}
    ]));
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0],
        Entity::Valid(Item {
            id: "a".into(),
            v: 1,
            name: "x".into()
        })
    );
    assert!(matches!(&items[1], Entity::Invalid { id, v: 2, .. } if id == "b"));
    assert!(matches!(&items[2], Entity::Invalid { id, v: 0, .. } if id == "invalid-2"));
    assert!(decode_collection::<Item>(&json!({"not": "an array"})).is_empty());
}
