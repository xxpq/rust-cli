use super::*;

#[test]
fn ids_split_at_the_last_at_sign() {
    assert_eq!(PluginId::parse("a@b@c"), Some(PluginId::new("a@b", "c")));
    assert_eq!(PluginId::parse("@c"), None);
    assert_eq!(PluginId::parse("a@"), None);
    assert_eq!(PluginId::new("kit", "mkt").to_string(), "kit@mkt");
    assert_eq!(PluginId::split_identifier("kit"), ("kit".to_owned(), None));
    assert_eq!(
        PluginId::split_identifier("kit@mkt"),
        ("kit".to_owned(), Some("mkt".to_owned()))
    );
}

#[test]
fn dependencies_without_a_marketplace_belong_to_the_dependent() {
    assert_eq!(
        PluginId::qualify("base", "mkt"),
        Some(PluginId::new("base", "mkt"))
    );
    assert_eq!(
        PluginId::qualify("base@other", "mkt"),
        Some(PluginId::new("base", "other"))
    );
}

#[test]
fn names_follow_node_patterns() {
    assert!(valid_name("kit-2.x_y"));
    assert!(!valid_name("Kit"));
    assert!(!valid_name("-kit"));
    assert!(!valid_name(""));
    assert!(!valid_name(&"a".repeat(129)));
    assert!(valid_name(&"a".repeat(128)));
    assert!(is_official_marketplace("zcode-plugins-official"));
    assert!(is_official_marketplace("claude-plugins-official"));
    assert!(!is_official_marketplace("mkt"));
}
