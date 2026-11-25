use serde_json::{Value, json};

use super::*;

const NOW: &str = "2026-11-12T00:00:00.000Z";

fn parsed(value: Value) -> Installed {
    Installed::parse(Some(value.to_string().as_bytes())).unwrap()
}

#[test]
fn installed_keeps_other_records_unknown_fields_and_top_level_keys() {
    let mut doc = parsed(json!({
        "version": 1,
        "extra": true,
        "plugins": [
            {"id": "other@mkt", "name": "other", "marketplace": "mkt", "version": "1", "installPath": "/o",
             "installedAt": "t0", "scope": "user", "cacheTransactionId": "keep-me", "future": 1},
            {"id": "kit@mkt", "name": "kit", "marketplace": "mkt", "version": "1", "installPath": "/old",
             "installedAt": "t1", "scope": "user", "cacheTransactionId": "drop-me", "mine": "x"},
            {"broken": true}
        ]
    }));
    doc.upsert(
        &NewRecord {
            id: PluginId::new("kit", "mkt"),
            version: "2.0.0".into(),
            install_path: "/new".into(),
            dependencies: None,
            source: Some(json!("./kit")),
        },
        NOW,
    );
    let out: Value = serde_json::from_str(&doc.to_text()).unwrap();
    assert_eq!(out["version"], 1);
    assert_eq!(out["extra"], true);
    assert_eq!(out["plugins"][0]["cacheTransactionId"], "keep-me");
    assert_eq!(out["plugins"][0]["future"], 1);
    let kit = &out["plugins"][1];
    assert_eq!(kit["installPath"], "/new");
    assert_eq!(kit["installedAt"], "t1");
    assert_eq!(kit["updatedAt"], NOW);
    assert_eq!(kit["mine"], "x");
    assert!(kit.get("cacheTransactionId").is_none());
    assert_eq!(out["plugins"][2], json!({"broken": true}));
    assert_eq!(doc.records().len(), 2);
    assert!(doc.to_text().ends_with("}\n"));
}

#[test]
fn installed_reads_the_object_format_and_refuses_unknown_versions() {
    let doc = parsed(
        json!({"plugins": {"kit@mkt": [{"installPath": "/k", "scope": "project", "version": "3"}]}}),
    );
    let records = doc.records();
    assert_eq!(records[0].id, "kit@mkt");
    assert_eq!(records[0].scope, "workspace");
    // 读者认任何版本；写者只写没写版本或版本是 1 的文件。
    let v2 = Installed::parse(Some(br#"{"version": 2, "plugins": {}}"#)).unwrap();
    assert!(v2.writable().is_err());
    assert!(doc.writable().is_ok());
    assert!(Installed::parse(Some(b"not json")).is_err());
    assert_eq!(Installed::parse(None).unwrap().records(), Vec::new());
}

#[test]
fn installed_remove_returns_the_record() {
    let mut doc = parsed(json!({"plugins": [
        {"id": "kit@mkt", "name": "kit", "marketplace": "mkt", "version": "1", "installPath": "/k", "installedAt": "t", "scope": "user"}
    ]}));
    assert!(doc.references("/k"));
    assert_eq!(doc.remove("kit@mkt").unwrap().install_path, "/k");
    assert!(doc.remove("kit@mkt").is_none());
    assert!(!doc.references("/k"));
}

#[test]
fn known_upsert_keeps_added_at_and_drops_failure_and_transaction() {
    let mut doc = KnownMarketplaces::parse(Some(json!({"version": 1, "marketplaces": [
        {"id": "mkt", "name": "mkt", "source": {"source": "directory", "path": "/m"}, "addedAt": "t0",
         "pluginCount": 1, "lastRefreshFailure": {"code": "x"}, "cacheTransactionId": "tx", "custom": 2}
    ]}).to_string().as_bytes()))
    .unwrap();
    doc.upsert(
        &NewKnown {
            id: "mkt".into(),
            source: json!({"source": "directory", "path": "/m"}),
            description: Some("d".into()),
            plugin_count: 3,
        },
        NOW,
    );
    let record = &serde_json::from_str::<Value>(&doc.to_text()).unwrap()["marketplaces"][0];
    assert_eq!(record["addedAt"], "t0");
    assert_eq!(record["lastUpdated"], NOW);
    assert_eq!(record["pluginCount"], 3);
    assert_eq!(record["custom"], 2);
    assert!(record.get("lastRefreshFailure").is_none());
    assert!(record.get("cacheTransactionId").is_none());
    assert!(doc.fail("mkt", "plugin_marketplace_invalid", "boom", NOW));
    assert!(!doc.fail("ghost", "c", "m", NOW));
    assert_eq!(
        doc.find("mkt").unwrap().refresh_failure.unwrap()["message"],
        "boom"
    );
}

#[test]
fn defaults_are_added_once() {
    let mut doc = KnownMarketplaces::parse(None).unwrap();
    assert!(doc.ensure_defaults(NOW));
    assert!(!doc.ensure_defaults(NOW));
    let ids: Vec<String> = doc.records().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, ["zcode-plugins-official", "claude-plugins-official"]);
    assert_eq!(
        doc.find("claude-plugins-official").unwrap().source.raw,
        json!({"source": "github", "repo": "anthropics/claude-plugins-official"})
    );
}

#[test]
fn transactions_are_found_at_any_depth() {
    let value = json!({"plugins": [{"id": "a", "cacheTransactionId": "tx-1"}]});
    assert!(contains_transaction(&value, "tx-1"));
    assert!(!contains_transaction(&value, "tx-2"));
}
