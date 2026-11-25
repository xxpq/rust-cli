use serde_json::{Value, json};

use super::*;

fn file(value: Value) -> SecretsFile {
    SecretsFile::parse(value.to_string().as_bytes()).unwrap()
}

#[test]
fn the_schema_is_strict() {
    assert!(SecretsFile::parse(br#"{"version": 1, "user": {}, "workspaces": {}}"#).is_some());
    assert!(SecretsFile::parse(br#"{"version": 2, "user": {}, "workspaces": {}}"#).is_none());
    assert!(
        SecretsFile::parse(br#"{"version": 1, "user": {}, "workspaces": {}, "x": 1}"#).is_none()
    );
    assert!(
        SecretsFile::parse(br#"{"version": 1, "user": {"a": {"k": {}}}, "workspaces": {}}"#)
            .is_none()
    );
    assert!(SecretsFile::parse(b"[]").is_none());
}

#[test]
fn set_clears_then_merges_and_folds_workspace_spellings() {
    let mut secrets = file(
        json!({"version": 1, "user": {"kit@m": {"old": "1", "keep": "2"}}, "workspaces": {
            "C:\\Work\\Repo": {"kit@m": {"a": "1"}},
            "c:/work/repo": {"other@m": {"b": "2"}}
        }}),
    );
    secrets.set(
        &Scope::User,
        "kit@m",
        json!({"new": 3}).as_object().unwrap(),
        &["old".to_owned()],
    );
    let key = project_key("C:\\Work\\Repo");
    assert_eq!(key, "c:/work/repo");
    secrets.set(
        &Scope::Workspace(key),
        "kit@m",
        json!({"c": true}).as_object().unwrap(),
        &[],
    );
    let out: Value = serde_json::from_str(&secrets.to_text()).unwrap();
    assert_eq!(out["user"], json!({"kit@m": {"keep": "2", "new": 3}}));
    assert_eq!(
        out["workspaces"],
        json!({"c:/work/repo": {"kit@m": {"a": "1", "c": true}, "other@m": {"b": "2"}}})
    );
}

#[test]
fn remove_everywhere_prunes_empty_layers() {
    let mut secrets = file(
        json!({"version": 1, "user": {"kit@m": {"k": "v"}}, "workspaces": {"/w": {"kit@m": {"k": "v"}}}}),
    );
    secrets.remove("kit@m", None);
    assert_eq!(
        serde_json::from_str::<Value>(&secrets.to_text()).unwrap(),
        json!({"version": 1, "user": {}, "workspaces": {}})
    );
}

#[test]
fn the_read_side_is_redacted() {
    let secrets = file(
        json!({"version": 1, "user": {"kit@m": {"token": "s3cret", "n": 2}}, "workspaces": {"/w": {"kit@m": {"t": "x"}}}}),
    );
    let view = secrets.redacted();
    assert_eq!(view.user["kit@m"]["token"].expose(), "s3cret");
    assert!(!format!("{:?}", view.user).contains("s3cret"));
    assert_eq!(view.user["kit@m"]["n"].expose(), "2");
    assert_eq!(view.workspaces[0].0, "/w");
}
