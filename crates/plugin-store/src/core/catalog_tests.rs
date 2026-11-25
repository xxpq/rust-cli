use serde_json::json;

use super::*;

#[test]
fn manifests_accept_arrays_and_maps_of_plugins() {
    let array = parse(&json!({
        "name": " mkt ",
        "metadata": {"pluginRoot": "./plugins", "description": "from metadata"},
        "allowCrossMarketplaceDependenciesOn": ["other"],
        "plugins": [
            {"name": "kit", "version": "1.0.0", "source": "./kit", "dependencies": ["base@^1.2", {"name": "x", "marketplace": "other"}]},
            {"name": ""},
            "junk"
        ]
    }))
    .unwrap();
    assert_eq!(array.name, "mkt");
    assert_eq!(array.description.as_deref(), Some("from metadata"));
    assert_eq!(array.plugin_root.as_deref(), Some("./plugins"));
    assert_eq!(array.cross_marketplace, ["other"]);
    assert_eq!(array.plugins.len(), 1);
    assert_eq!(array.plugins[0].dependencies, ["base", "x@other"]);
    assert!(array.plugins[0].strict);
    assert_eq!(array.raw["name"], "mkt");

    let map = parse(&json!({"name": "mkt", "plugins": {"kit": {"strict": false}}})).unwrap();
    assert!(!map.find("kit").unwrap().strict);
    assert_eq!(
        map.raw["plugins"],
        json!([{"name": "kit", "strict": false}])
    );
}

#[test]
fn invalid_names_reject_the_manifest() {
    assert!(parse(&json!({"name": "Bad Name", "plugins": []})).is_none());
    assert!(parse(&json!({"plugins": []})).is_none());
    assert!(parse(&json!([])).is_none());
}

#[test]
fn entries_carry_the_store_listing_component_types_and_featured_names() {
    let catalog = parse(&serde_json::json!({
        "name": "mkt",
        "featured": ["kit", " ", 3],
        "plugins": [{
            "name": "kit",
            "displayName": "Kit",
            "displayName_i18n": {"zh-CN": "工具箱"},
            "author": {"name": "Dev", "url": "https://example.com/dev"},
            "examplePrompts": ["hi", ""],
            "requiresPaidPlan": "true",
            "skills": "./skills",
            "mcpServers": {},
            "componentTypes": ["ui", "other"]
        }, {"name": "bare"}]
    }))
    .unwrap();
    assert_eq!(catalog.featured, ["kit"]);
    let kit = catalog.find("kit").unwrap();
    let listing = kit.listing().unwrap();
    assert_eq!(listing.display_name.as_deref(), Some("Kit"));
    assert_eq!(listing.display_name_i18n["zh-CN"], "工具箱");
    assert_eq!(listing.author.as_deref(), Some("Dev"));
    assert_eq!(
        listing.author_url.as_deref(),
        Some("https://example.com/dev")
    );
    assert_eq!(listing.example_prompts, ["hi"]);
    assert!(!listing.requires_paid_plan);
    assert_eq!(kit.component_types(), ["skill", "mcp", "ui"]);
    let bare = catalog.find("bare").unwrap();
    assert_eq!(bare.listing(), None);
    assert!(bare.component_types().is_empty());
}
