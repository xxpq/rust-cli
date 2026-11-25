//! 安装、更新、卸载与孤儿目录（plugin-store.md §5、§6）。
#![allow(clippy::disallowed_methods, reason = "测试直接查看临时目录")]

use std::collections::BTreeSet;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::fixture::{Fixture, read_json, serve};
use super::super::market::tests::{add, market};
use super::*;
use crate::core::ORPHAN_TTL_MS;
use crate::core::source::MarketplaceSource;

const NOW: u64 = 1_800_000_000_000;

async fn plugin(fixture: &Fixture, dir: &str, name: &str, version: &str) {
    fixture
        .json(
            &format!("{dir}/.claude-plugin/plugin.json"),
            &json!({"name": name, "version": version}),
        )
        .await;
    fixture
        .write(
            &format!("{dir}/skills/{name}/SKILL.md"),
            format!("---\ndescription: {name} {version}\n---\n"),
        )
        .await;
}

async fn install(store: &LocalStore, id: &str) -> Result<Vec<Committed>, StoreError> {
    let cancel = CancellationToken::new();
    let prepared = store
        .prepare_install(&PluginId::parse(id).unwrap(), &cancel)
        .await?;
    let locked = store.lock(&cancel).await?;
    let committed = locked.commit_install(prepared, NOW).await;
    locked.release().await;
    committed
}

/// 刷新目录市场（清单缓存是加的时候复制的，改了来源要刷新才看得到，同 Node 的 `update`）。
async fn refresh(store: &LocalStore, fixture: &Fixture, name: &str) {
    let source = MarketplaceSource::local(fixture.path(name).to_string_lossy().into_owned(), true);
    let cancel = CancellationToken::new();
    let staged = store
        .prepare_marketplace(source, Some(name), &cancel)
        .await
        .unwrap();
    let locked = store.lock(&cancel).await.unwrap();
    locked.commit_marketplace(staged, NOW).await.unwrap();
    locked.release().await;
}

async fn setup(fixture: &Fixture) -> LocalStore {
    let store = fixture.store().await;
    plugin(fixture, "mkt/kit", "kit", "1.0.0").await;
    plugin(fixture, "mkt/base", "base", "0.1.0").await;
    let source = market(fixture, "mkt", json!([
        {"name": "kit", "source": "./kit", "dependencies": ["base"]},
        {"name": "base", "source": "./base"},
        {"name": "loose", "source": "./loose", "strict": false, "description": "Loose", "category": "x"}
    ]))
    .await;
    add(&store, source).await.unwrap();
    store
}

#[tokio::test]
async fn install_commits_the_closure_into_immutable_directories() {
    let fixture = Fixture::new();
    let store = setup(&fixture).await;
    let committed = install(&store, "kit@mkt").await.unwrap();
    let ids: Vec<String> = committed.iter().map(|c| c.id.to_string()).collect();
    assert_eq!(ids, ["base@mkt", "kit@mkt"]);
    let kit = &committed[1];
    let name = kit
        .install_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("1.0.0-") && name.len() == "1.0.0-".len() + 12,
        "{name}"
    );
    assert!(kit.install_path.join("skills/kit/SKILL.md").is_file());
    let marker = read_json(&kit.install_path.join(".zcode-package.json")).await;
    assert_eq!(marker["pluginId"], "kit@mkt");
    assert!(marker["digest"].as_str().unwrap().starts_with("sha256:"));
    let records = read_json(&store.layout().installed()).await;
    assert_eq!(records["version"], 1);
    let kit_record = &records["plugins"][1];
    assert_eq!(
        kit_record["installPath"],
        kit.install_path.to_string_lossy().as_ref()
    );
    assert_eq!(kit_record["scope"], "user");
    assert_eq!(kit_record["dependencies"], json!(["base"]));
    assert_eq!(kit_record["source"], "./kit");
    // 暂存都提交或删掉了，没有残留。
    let leftovers = rust_cli_fs::subdirectories(&store.layout().plugin_dir("mkt", "kit"))
        .await
        .unwrap();
    assert_eq!(leftovers, [name]);
}

#[tokio::test]
async fn reinstalling_the_same_content_reuses_the_directory_and_updates_orphan_the_old_one() {
    let fixture = Fixture::new();
    let store = setup(&fixture).await;
    let first = install(&store, "kit@mkt").await.unwrap().pop().unwrap();
    let again = install(&store, "kit@mkt").await.unwrap().pop().unwrap();
    assert_eq!(first.install_path, again.install_path);
    assert!(!first.install_path.join(".orphaned_at").exists());
    let installed_at =
        read_json(&store.layout().installed()).await["plugins"][1]["installedAt"].clone();

    plugin(&fixture, "mkt/kit", "kit", "1.1.0").await;
    refresh(&store, &fixture, "mkt").await;
    let updated = install(&store, "kit@mkt").await.unwrap().pop().unwrap();
    assert_ne!(updated.install_path, first.install_path);
    assert_eq!(updated.previous.unwrap().version, "1.0.0");
    // 旧目录不删，打孤儿标记（A3）；installedAt 保留。
    assert!(first.install_path.join("skills").is_dir());
    assert_eq!(
        std::fs::read_to_string(first.install_path.join(".orphaned_at")).unwrap(),
        NOW.to_string()
    );
    assert_eq!(
        read_json(&store.layout().installed()).await["plugins"][1]["installedAt"],
        installed_at
    );
}

#[tokio::test]
async fn strict_false_entries_get_a_synthesized_manifest() {
    let fixture = Fixture::new();
    let store = setup(&fixture).await;
    fixture
        .write("mkt/loose/skills/x/SKILL.md", "---\ndescription: X\n---\n")
        .await;
    refresh(&store, &fixture, "mkt").await;
    let loose = install(&store, "loose@mkt").await.unwrap().pop().unwrap();
    let manifest = read_json(&loose.install_path.join(".claude-plugin/plugin.json")).await;
    assert_eq!(
        manifest,
        json!({"name": "loose", "description": "Loose", "version": "0.0.0"})
    );
}

#[tokio::test]
async fn zip_entries_are_downloaded_checked_and_extracted() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let bytes = super::super::zip::tests::make_zip(
        &[
            (
                "kit/.claude-plugin/plugin.json",
                Some(br#"{"name":"kit","version":"2.0.0"}"#),
            ),
            ("kit/skills/a/SKILL.md", Some(b"---\ndescription: A\n---\n")),
        ],
        &[],
    );
    let sha = rust_cli_base::hash::sha256_hex(&bytes);
    let base = serve(200, bytes).await;
    let source = market(&fixture, "zips", json!([
        {"name": "kit", "source": {"source": "url", "type": "zip", "url": format!("{base}/kit.zip"), "sha256": sha}},
        {"name": "bad", "source": {"source": "url", "type": "zip", "url": format!("{base}/kit.zip"), "sha256": "0".repeat(64)}}
    ]))
    .await;
    add(&store, source).await.unwrap();
    let kit = install(&store, "kit@zips").await.unwrap().pop().unwrap();
    assert_eq!(kit.version, "2.0.0");
    assert!(kit.install_path.join("skills/a/SKILL.md").is_file());
    let error = install(&store, "bad@zips").await.unwrap_err();
    assert!(error.to_string().contains("sha256 mismatch"), "{error}");
    assert_eq!(error.code(), "plugin_archive_fetch_failed");
}

#[tokio::test]
async fn cancelling_before_the_lock_changes_nothing() {
    let fixture = Fixture::new();
    let store = setup(&fixture).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = store
        .prepare_install(&PluginId::new("kit", "mkt"), &cancel)
        .await
        .unwrap_err();
    assert_eq!(error, StoreError::Cancelled);
    assert!(!store.layout().installed().exists());
    let leftovers = rust_cli_fs::subdirectories(&store.layout().plugin_dir("mkt", "base"))
        .await
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn concurrent_installs_in_one_process_keep_both_records() {
    let fixture = Fixture::new();
    let first = setup(&fixture).await;
    let second = fixture.store().await;
    let (a, b) = tokio::join!(install(&first, "kit@mkt"), install(&second, "base@mkt"));
    a.unwrap();
    b.unwrap();
    let ids: BTreeSet<String> = first
        .installed_records()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        ids,
        BTreeSet::from(["base@mkt".to_owned(), "kit@mkt".to_owned()])
    );
}

#[tokio::test]
async fn uninstall_removes_the_record_and_orphans_the_directory() {
    let fixture = Fixture::new();
    let store = setup(&fixture).await;
    let kit = install(&store, "kit@mkt").await.unwrap().pop().unwrap();
    let cancel = CancellationToken::new();
    let locked = store.lock(&cancel).await.unwrap();
    let removed = locked
        .remove_install("kit@mkt", NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(removed.version, "1.0.0");
    assert!(
        locked
            .remove_install("kit@mkt", NOW)
            .await
            .unwrap()
            .is_none()
    );
    locked.release().await;
    assert!(kit.install_path.join(".orphaned_at").is_file());
    let ids: Vec<String> = store
        .installed_records()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, ["base@mkt"]);
}

#[tokio::test]
async fn sweeping_expires_orphans_skips_in_use_and_never_marks_foreign_directories() {
    let fixture = Fixture::new();
    let store = setup(&fixture).await;
    let kit = install(&store, "kit@mkt").await.unwrap().pop().unwrap();
    let cancel = CancellationToken::new();
    let locked = store.lock(&cancel).await.unwrap();
    locked.remove_install("kit@mkt", NOW).await.unwrap();
    // Node 播种的目录：不在记录里、没有摘要标记，不补标记也不删。
    let node_dir = store
        .layout()
        .plugin_dir("zcode-plugins-official", "pdf")
        .join("1.0.0");
    rust_cli_fs::create_dir_all(&node_dir).await.unwrap();
    let later = NOW + ORPHAN_TTL_MS + 1;
    let in_use = BTreeSet::from([kit.install_path.clone()]);
    assert!(
        locked
            .sweep_orphans(later, &in_use)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        locked.sweep_orphans(later, &BTreeSet::new()).await.unwrap(),
        std::slice::from_ref(&kit.install_path)
    );
    assert!(node_dir.is_dir());
    assert!(!node_dir.join(".orphaned_at").exists());
    locked.release().await;
}

#[tokio::test]
async fn records_with_an_unknown_version_are_not_rewritten() {
    let fixture = Fixture::new();
    let store = setup(&fixture).await;
    let path = store.layout().installed();
    rust_cli_fs::write_file(&path, br#"{"version": 2, "plugins": {}}"#)
        .await
        .unwrap();
    let error = install(&store, "kit@mkt").await.unwrap_err();
    assert!(error.to_string().contains("unsupported version"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"version": 2, "plugins": {}}"#
    );
}
