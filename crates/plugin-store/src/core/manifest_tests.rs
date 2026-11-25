use serde_json::json;

use super::*;
use crate::core::catalog::parse;

fn entry(value: serde_json::Value) -> Entry {
    parse(&json!({"name": "mkt", "plugins": [value]}))
        .unwrap()
        .plugins
        .remove(0)
}

#[test]
fn synthesized_manifests_drop_catalog_fields() {
    let manifest = synthesize(&entry(json!({
        "name": "kit", "source": "./kit", "strict": false, "category": "x", "icon": "i.png",
        "description": "Kit", "author": {"name": "Dev"}
    })));
    assert_eq!(
        manifest,
        json!({"name": "kit", "description": "Kit", "author": {"name": "Dev"}, "version": "0.0.0"})
    );
}

#[test]
fn versions_prefer_the_manifest_then_the_entry() {
    let kit = entry(json!({"name": "kit", "version": "1.0.0"}));
    assert_eq!(
        installed_version(Some(&json!({"version": "2.0.0"})), &kit),
        "2.0.0"
    );
    assert_eq!(
        installed_version(Some(&json!({"version": " "})), &kit),
        "1.0.0"
    );
    assert_eq!(
        installed_version(None, &entry(json!({"name": "kit"}))),
        "0.0.0"
    );
}

#[test]
fn names_must_follow_the_pattern() {
    assert_eq!(name(&json!({"name": " kit "})).unwrap(), "kit");
    assert_eq!(
        name(&json!({"name": "Kit"})).unwrap_err(),
        "Invalid plugin name: Kit"
    );
    assert!(name(&json!([])).is_err());
}
