use serde_json::json;

use super::*;

fn remote(input: &str) -> Value {
    match classify_input(input).unwrap() {
        SourceInput::Remote(source) => source.raw,
        SourceInput::Path(path) => panic!("path {path}"),
    }
}

#[test]
fn inputs_map_to_node_source_shapes() {
    assert_eq!(
        remote("https://example.com/x/repo.git#main"),
        json!({"source": "git", "url": "https://example.com/x/repo.git", "ref": "main"})
    );
    assert_eq!(
        remote("https://github.com/acme/plugins"),
        json!({"source": "git", "url": "https://github.com/acme/plugins.git"})
    );
    assert_eq!(
        remote("https://example.com/marketplace.json"),
        json!({"source": "url", "url": "https://example.com/marketplace.json"})
    );
    assert_eq!(
        remote("git@example.com:acme/plugins.git"),
        json!({"source": "git", "url": "git@example.com:acme/plugins.git"})
    );
    assert_eq!(
        remote("acme/plugins@v2"),
        json!({"source": "github", "repo": "acme/plugins", "ref": "v2"})
    );
    assert_eq!(
        classify_input("./market").unwrap(),
        SourceInput::Path("./market".to_owned())
    );
    assert_eq!(
        classify_input("C:\\m").unwrap(),
        SourceInput::Path("C:\\m".to_owned())
    );
    assert!(classify_input("plain").is_err());
    assert!(classify_input("  ").is_err());
}

#[test]
fn sparse_only_applies_to_git_sources() {
    let git = MarketplaceSource::from_json(&json!({"source": "github", "repo": "a/b"}));
    let git = git.with_sparse(&["plugins".to_owned()]).unwrap();
    assert_eq!(git.raw["sparsePaths"], json!(["plugins"]));
    let dir = MarketplaceSource::local("/m".to_owned(), true);
    assert!(dir.with_sparse(&["x".to_owned()]).is_err());
    assert_eq!(
        MarketplaceSource::default_from("anthropics/claude-plugins-official").raw,
        json!({"source": "github", "repo": "anthropics/claude-plugins-official"})
    );
}

#[test]
fn records_round_trip_unknown_kinds() {
    let npm = MarketplaceSource::from_json(&json!({"source": "npm", "package": "x"}));
    assert_eq!(npm.kind, MarketplaceKind::Unsupported("npm".to_owned()));
    assert_eq!(npm.raw, json!({"source": "npm", "package": "x"}));
}

#[test]
fn plugin_sources_follow_node() {
    assert_eq!(plugin_source(None, "k").unwrap(), PluginSource::ByName);
    assert_eq!(
        plugin_source(Some(&json!("./k")), "k").unwrap(),
        PluginSource::Relative("./k".into())
    );
    assert!(matches!(
        plugin_source(Some(&json!({"source": "url", "type": "zip", "url": "https://cdn.example.com/k.zip", "sha256": "ab"})), "k").unwrap(),
        PluginSource::Zip { .. }
    ));
    assert!(matches!(
        plugin_source(
            Some(&json!({"source": "github", "repo": "a/b", "sha": "123"})),
            "k"
        )
        .unwrap(),
        PluginSource::Repo { sha: Some(_), .. }
    ));
    for unsupported in [
        json!({"source": "npm", "package": "x"}),
        json!("filesystem"),
    ] {
        assert!(matches!(
            plugin_source(Some(&unsupported), "k"),
            Err(StoreError::Unsupported(_))
        ));
    }
    assert!(matches!(
        plugin_source(
            Some(&json!({"source": "url", "type": "zip", "url": "u"})),
            "k"
        ),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn github_urls_are_public_https_repositories() {
    assert_eq!(
        github_repository("https://github.com/acme/kit.git"),
        Some(("acme".to_owned(), "kit".to_owned()))
    );
    assert_eq!(github_repository("http://github.com/acme/kit"), None);
    assert_eq!(
        github_repository("https://github.com/acme/kit/tree/x"),
        None
    );
    assert_eq!(github_repository("https://example.com/acme/kit"), None);
}
