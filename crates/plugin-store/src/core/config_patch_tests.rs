use serde_json::{Map, Value, json};

use super::*;

fn config(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn enable_moves_the_key_last_and_keeps_other_sections() {
    let mut c = config(
        json!({"ui": {"locale": "zh-CN"}, "plugins": {"enabledPlugins": {"a@m": true, "b@m": true}}}),
    );
    assert!(set_enabled(&mut c, "a@m", false));
    assert_eq!(
        Value::Object(c.clone()),
        json!({"ui": {"locale": "zh-CN"}, "plugins": {"enabledPlugins": {"b@m": true, "a@m": false}}})
    );
    assert!(clear_enabled(&mut c, "a@m"));
    assert!(!clear_enabled(&mut c, "a@m"));
}

#[test]
fn default_enable_only_fills_missing_keys() {
    let mut c = config(json!({"plugins": {"enabledPlugins": {"a@m": false}}}));
    assert_eq!(
        default_enable(&mut c, &["a@m".into(), "b@m".into()]),
        ["b@m"]
    );
    assert_eq!(
        c["plugins"]["enabledPlugins"],
        json!({"a@m": false, "b@m": true})
    );
    let mut empty = Map::new();
    assert_eq!(default_enable(&mut empty, &[]), Vec::<String>::new());
    assert!(empty.is_empty());
}

#[test]
fn remove_and_suppress_follow_node() {
    let mut c = config(
        json!({"plugins": {"enabledPlugins": {"a@m": true}, "options": {"a@m": {"k": 1}}, "suppressedBuiltins": ["x"]}}),
    );
    assert!(remove_plugin(&mut c, "a@m"));
    assert!(!remove_plugin(&mut c, "a@m"));
    assert!(suppress(&mut c, "pdf@zcode-plugins-official"));
    assert!(!suppress(&mut c, "pdf@zcode-plugins-official"));
    assert_eq!(
        c["plugins"]["suppressedBuiltins"],
        json!(["x", "pdf@zcode-plugins-official"])
    );
    assert!(unsuppress(&mut c, "pdf@zcode-plugins-official"));
    assert!(!unsuppress(&mut c, "pdf@zcode-plugins-official"));
    assert_eq!(c["plugins"]["suppressedBuiltins"], json!(["x"]));
}

#[test]
fn options_merge_by_key_after_clearing() {
    let mut c =
        config(json!({"plugins": {"options": {"a@m": {"keep": 1, "drop": 2, "nested": {}}}}}));
    let set = config(json!({"new": "v"}));
    assert!(patch_options(&mut c, "a@m", &set, &["drop".to_owned()]));
    assert_eq!(
        c["plugins"]["options"]["a@m"],
        json!({"keep": 1, "new": "v"})
    );
    assert_eq!(
        options_of(&c, "a@m"),
        config(json!({"keep": 1, "new": "v"}))
    );
}
