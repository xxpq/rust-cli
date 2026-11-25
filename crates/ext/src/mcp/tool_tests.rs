//! 工具自己的不延迟标记（docs/specs/rust-cli/ext.md §3.1、D35）：`_meta` 里新旧两个键任一为 true 就不延迟。
use std::sync::Weak;

use serde_json::{Value, json};

use super::{McpTool, arguments, hidden_from_model};

fn always_load(listed: &Value, server_always_load: bool) -> bool {
    McpTool::from_listed("files", listed, server_always_load, Weak::new())
        .unwrap()
        .info
        .always_load
}

fn with_meta(meta: Value) -> Value {
    json!({"name": "read", "inputSchema": {"type": "object"}, "_meta": meta})
}

#[test]
fn either_meta_key_keeps_the_tool_loaded() {
    assert!(always_load(
        &with_meta(json!({"zcode/alwaysLoad": true})),
        false
    ));
    // 旧键：Node 与已有的 MCP 服务器用它。
    assert!(always_load(
        &with_meta(json!({"anthropic/alwaysLoad": true})),
        false
    ));
}

#[test]
fn other_values_and_keys_leave_the_tool_deferred() {
    for meta in [
        json!({"anthropic/alwaysLoad": false}),
        json!({"anthropic/alwaysLoad": "true"}),
        json!({"zcode/alwaysLoad": 1}),
        json!({"other/alwaysLoad": true}),
        json!({"alwaysLoad": true}),
        json!(null),
    ] {
        assert!(!always_load(&with_meta(meta.clone()), false), "{meta}");
    }
    assert!(!always_load(&json!({"name": "read"}), false));
}

#[test]
fn the_server_option_does_not_look_at_the_tool() {
    assert!(always_load(&json!({"name": "read"}), true));
    assert!(always_load(
        &with_meta(json!({"anthropic/alwaysLoad": false})),
        true
    ));
}

/// MCP 工具给分类器看的参数（auto-classifier.md §15.3）：`k=v`，对象与数组写成 JSON；没有参数时是工具原名。
#[test]
fn mcp_arguments_are_listed_for_the_classifier() {
    use rust_cli_kernel_api::classifier::ClassifierInput;
    assert_eq!(
        arguments(
            &json!({"path": "/w/a", "depth": 2, "flags": ["x"], "opts": {"a": true}}),
            "list"
        ),
        ClassifierInput::Text(r#"path=/w/a depth=2 flags=["x"] opts={"a":true}"#.to_owned())
    );
    assert_eq!(
        arguments(&json!({}), "list"),
        ClassifierInput::Text("list".to_owned())
    );
}

/// 只给界面的工具不给模型（ext.md §4.11 的判定；旧代码全部给模型）。
#[test]
fn tools_without_model_visibility_are_kept_from_the_model() {
    let hidden = |meta: Value| hidden_from_model(&with_meta(meta));
    assert!(!hidden_from_model(&json!({"name": "read"})));
    assert!(!hidden(json!({"ui": null})));
    assert!(!hidden(json!({"ui": {"resourceUri": "ui://x"}})));
    assert!(!hidden(json!({"ui": {"visibility": null}})));
    assert!(!hidden(json!({"ui": {"visibility": ["model", "app"]}})));
    assert!(hidden(json!({"ui": {"visibility": ["app"]}})));
    // 认不出的写法也不给（与 Node 的缺省不同，D46）。
    assert!(hidden(json!({"ui": {"visibility": []}})));
    assert!(hidden(json!({"ui": {"visibility": "model"}})));
    assert!(hidden(json!({"ui": "app"})));
}
