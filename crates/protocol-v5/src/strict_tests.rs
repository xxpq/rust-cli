//! 严格读（03 §3.8）：严格变体只多“不许多余的键”这一条，别的照给 rust 的 schema。

use serde_json::json;

use super::*;
use crate::generated::contract::{METHOD_PARAMS, SUBSCRIBE};

#[test]
fn objects_with_properties_reject_unknown_keys_while_records_and_opaque_stay_open() {
    let schema = json!({
        "type": "object",
        "properties": {
            "a": {"type": "object", "properties": {"b": {"type": "string"}}},
            "m": {"type": "object", "additionalProperties": {"type": "object", "properties": {"c": {"type": "integer"}}}},
            "x": {}
        },
        "required": ["a"]
    });
    let strict = strict_variant(&schema);
    assert_eq!(strict["additionalProperties"], json!(false));
    assert_eq!(
        strict["properties"]["a"]["additionalProperties"],
        json!(false)
    );
    // record：值的类型照旧（只是值里的对象也严格），键不受限
    assert_eq!(
        strict["properties"]["m"]["additionalProperties"]["additionalProperties"],
        json!(false)
    );
    assert!(strict["properties"]["m"].get("properties").is_none());
    // 不透明位置不动
    assert_eq!(strict["properties"]["x"], json!({}));
}

#[test]
fn branches_and_array_items_are_strict_too() {
    let schema = json!({
        "anyOf": [
            {"type": "object", "properties": {"k": {"const": "a"}}},
            {"type": "array", "items": {"type": "object", "properties": {"v": {"type": "string"}}}}
        ]
    });
    let strict = strict_variant(&schema);
    assert_eq!(strict["anyOf"][0]["additionalProperties"], json!(false));
    assert_eq!(
        strict["anyOf"][1]["items"]["additionalProperties"],
        json!(false)
    );
}

/// 生成的 schema 都能编译（server_tests 也证明）；这里证明方法表里的每个名字都找得到、严格变体也能编译——
/// 否则这个方法在严格链路上不会被校验
#[test]
fn every_core_method_has_a_compiled_strict_schema() {
    for (method, name) in METHOD_PARAMS {
        assert!(params_schema(method).is_some(), "{method} → {name}");
    }
}

#[test]
fn an_unknown_field_fails_with_its_path_and_a_known_shape_passes() {
    let error = check(
        SUBSCRIBE,
        &json!({"sub": "a", "topic": "agent", "extra": 1}),
    )
    .unwrap_err();
    assert!(error.contains("extra"), "{error}");
    assert!(check(SUBSCRIBE, &json!({"sub": "a", "topic": "agent"})).is_ok());
}
