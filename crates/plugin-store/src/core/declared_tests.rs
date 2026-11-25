use serde_json::json;

use super::*;
use crate::core::id::OFFICIAL_MARKETPLACE as OFFICIAL;

fn source(path: &str) -> MarketplaceSource {
    MarketplaceSource::from_json(&json!({"source": "directory", "path": path}))
}

fn known(id: &str, path: &str) -> Known {
    Known {
        id: id.to_owned(),
        name: id.to_owned(),
        source: source(path),
        description: None,
        added_at: None,
        last_updated: None,
        plugin_count: 3,
        refresh_failure: None,
    }
}

#[test]
fn declarations_join_the_known_records_by_node_rules() {
    let declared: BTreeMap<String, MarketplaceSource> = [
        ("same".to_owned(), source("/Users/dev/same")),
        ("moved".to_owned(), source("/Users/dev/new")),
        ("only".to_owned(), source("/Users/dev/only")),
        (OFFICIAL.to_owned(), source("/Users/dev/fake")),
    ]
    .into();
    let (listed, diagnostics) = merge(
        vec![
            known("same", "/Users/dev/same"),
            known("moved", "/Users/dev/old"),
        ],
        &declared,
    );
    let by = |id: &str| listed.iter().find(|item| item.record.id == id).unwrap();
    assert!(by("same").cached && by("same").record.plugin_count == 3);
    assert!(!by("moved").cached && by("moved").record.plugin_count == 0);
    assert_eq!(by("moved").record.source, source("/Users/dev/new"));
    assert!(!by("only").cached);
    assert!(listed.iter().all(|item| item.record.id != OFFICIAL));
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, code::DECLARATION_RESERVED);
}

#[test]
fn explicit_actions_materialize_new_declarations_and_refuse_repointing() {
    let declared: BTreeMap<String, MarketplaceSource> = [
        ("new".to_owned(), source("/Users/dev/new")),
        ("moved".to_owned(), source("/Users/dev/new")),
        ("same".to_owned(), source("/Users/dev/same")),
    ]
    .into();
    let records = [
        known("moved", "/Users/dev/old"),
        known("same", "/Users/dev/same"),
    ];
    assert_eq!(
        to_materialize(&records, &declared, "new").unwrap(),
        Some(&source("/Users/dev/new"))
    );
    assert_eq!(to_materialize(&records, &declared, "same").unwrap(), None);
    assert_eq!(to_materialize(&records, &declared, "other").unwrap(), None);
    assert!(matches!(
        to_materialize(&records, &declared, "moved"),
        Err(StoreError::Invalid(message)) if message.contains("conflicts with an existing Host source")
    ));
}
