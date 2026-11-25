//! 市场的加、刷新、删与清单缓存的替换（plugin-store.md §9.2、§6.4）。

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::fixture::{Fixture, read_json, serve};
use super::*;
use crate::core::source::{MarketplaceSource, SourceInput, classify_input};

const NOW: u64 = 1_800_000_000_000;

pub(crate) async fn market(
    fixture: &Fixture,
    name: &str,
    plugins: serde_json::Value,
) -> MarketplaceSource {
    fixture
        .json(
            &format!("{name}/.claude-plugin/marketplace.json"),
            &json!({"name": name, "plugins": plugins}),
        )
        .await;
    MarketplaceSource::local(fixture.path(name).to_string_lossy().into_owned(), true)
}

pub(crate) async fn add(
    store: &LocalStore,
    source: MarketplaceSource,
) -> Result<Known, StoreError> {
    let cancel = CancellationToken::new();
    let staged = store.prepare_marketplace(source, None, &cancel).await?;
    let locked = store.lock(&cancel).await?;
    let known = locked.commit_marketplace(staged, NOW).await;
    locked.release().await;
    known
}

#[tokio::test]
async fn a_directory_marketplace_is_copied_and_recorded() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    fixture
        .write("mkt/kit/.claude-plugin/plugin.json", r#"{"name":"kit"}"#)
        .await;
    let source = market(&fixture, "mkt", json!([{"name": "kit", "source": "./kit"}])).await;
    let known = add(&store, source).await.unwrap();
    assert_eq!(known.id, "mkt");
    assert_eq!(known.plugin_count, 1);
    let dir = store.layout().marketplace_dir("mkt");
    assert!(rust_cli_fs::is_file(&dir.join("kit/.claude-plugin/plugin.json")).await);
    assert_eq!(
        read_json(&dir.join("marketplace.json")).await["plugins"][0]["name"],
        "kit"
    );
    let record = &read_json(&store.layout().known()).await["marketplaces"][0];
    assert_eq!(
        record["source"],
        json!({"source": "directory", "path": fixture.path("mkt").to_string_lossy()})
    );
    assert_eq!(
        store
            .catalog("mkt")
            .await
            .unwrap()
            .unwrap()
            .catalog
            .plugins
            .len(),
        1
    );
}

#[tokio::test]
async fn refresh_replaces_the_cache_and_keeps_added_at() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let source = market(&fixture, "mkt", json!([{"name": "a"}])).await;
    add(&store, source.clone()).await.unwrap();
    let first = read_json(&store.layout().known()).await["marketplaces"][0]["addedAt"].clone();
    market(&fixture, "mkt", json!([{"name": "a"}, {"name": "b"}])).await;
    let cancel = CancellationToken::new();
    let staged = store
        .prepare_marketplace(source, Some("mkt"), &cancel)
        .await
        .unwrap();
    let locked = store.lock(&cancel).await.unwrap();
    let known = locked.commit_marketplace(staged, NOW + 1).await.unwrap();
    locked.release().await;
    assert_eq!(known.plugin_count, 2);
    assert_eq!(
        read_json(&store.layout().known()).await["marketplaces"][0]["addedAt"],
        first
    );
    let names = rust_cli_fs::subdirectories(&store.layout().marketplaces())
        .await
        .unwrap();
    assert_eq!(names, ["mkt"]);
}

#[tokio::test]
async fn a_crash_between_the_two_renames_is_recovered_by_the_next_writer() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let source = market(&fixture, "mkt", json!([{"name": "a"}])).await;
    add(&store, source.clone()).await.unwrap();
    // 崩溃：目标已改名成旧目录，暂存还没改名进来。
    let target = store.layout().marketplace_dir("mkt");
    let old = store.layout().marketplaces().join(".mkt.rust-old-1-1");
    rust_cli_fs::rename(&target, &old).await.unwrap();
    let cancel = CancellationToken::new();
    let locked = store.lock(&cancel).await.unwrap();
    recover_old(&store.layout().marketplaces(), &target)
        .await
        .unwrap();
    locked.release().await;
    assert!(rust_cli_fs::is_file(&target.join("marketplace.json")).await);
    assert!(!rust_cli_fs::is_dir(&old).await);
}

#[tokio::test]
async fn official_ids_are_reserved_and_refresh_names_must_match() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let source = market(&fixture, "zcode-plugins-official", json!([])).await;
    let error = add(&store, source).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("reserved for the official marketplace"),
        "{error}"
    );
    let other = market(&fixture, "other", json!([])).await;
    let cancel = CancellationToken::new();
    let error = store
        .prepare_marketplace(other, Some("mkt"), &cancel)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("must provide mkt"), "{error}");
    // 暂存都删了。
    let names = rust_cli_fs::subdirectories(&store.layout().marketplaces())
        .await
        .unwrap();
    assert!(names.is_empty(), "{names:?}");
}

#[tokio::test]
async fn url_marketplaces_and_the_official_catalog() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let base = serve(
        200,
        json!({"name": "zcode-plugins-official", "plugins": [{"name": "kit"}]})
            .to_string()
            .into_bytes(),
    )
    .await;
    let SourceInput::Remote(source) = classify_input(&format!("{base}/marketplace.json")).unwrap()
    else {
        panic!("remote");
    };
    let cancel = CancellationToken::new();
    let staged = store
        .prepare_marketplace(source, Some("zcode-plugins-official"), &cancel)
        .await
        .unwrap();
    let locked = store.lock(&cancel).await.unwrap();
    locked.commit_marketplace(staged, NOW).await.unwrap();
    locked.release().await;
    // 官方市场的清单只写 rust 自己的文件（A5）。
    assert!(rust_cli_fs::is_file(&store.layout().official_catalog()).await);
    assert!(!rust_cli_fs::is_dir(&store.layout().marketplace_dir("zcode-plugins-official")).await);
    assert_eq!(
        store
            .catalog("zcode-plugins-official")
            .await
            .unwrap()
            .unwrap()
            .catalog
            .plugins[0]
            .name,
        "kit"
    );
}

#[tokio::test]
async fn failures_defaults_and_removal() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let cancel = CancellationToken::new();
    let locked = store.lock(&cancel).await.unwrap();
    locked.ensure_default_marketplaces(NOW).await.unwrap();
    locked
        .record_refresh_failure(
            "claude-plugins-official",
            &StoreError::Fetch("offline".into()),
            NOW,
        )
        .await
        .unwrap();
    locked
        .record_refresh_failure("claude-plugins-official", &StoreError::Cancelled, NOW)
        .await
        .unwrap();
    let records = locked.known_records().await.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[1].refresh_failure.as_ref().unwrap()["message"],
        "offline"
    );
    assert!(
        locked
            .remove_marketplace("claude-plugins-official")
            .await
            .unwrap()
    );
    assert!(
        !locked
            .remove_marketplace("claude-plugins-official")
            .await
            .unwrap()
    );
    locked.release().await;
    assert_eq!(store.known_marketplaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn unsupported_sources_say_so() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let npm = MarketplaceSource::from_json(&json!({"source": "npm", "package": "x"}));
    let error = store
        .prepare_marketplace(npm, None, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "plugin_marketplace_source_unsupported");
}
