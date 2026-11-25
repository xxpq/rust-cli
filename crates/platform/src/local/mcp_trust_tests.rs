//! 工作区 MCP 的信任记录的读写（18-mcp-trust.md §12.3）。
#![allow(clippy::disallowed_methods, reason = "测试读回写出的文件与权限")]
use super::*;
use crate::test_support::write;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const AT: &str = "2026-11-05T08:00:00.000Z";
const REPO: &str = "/Users/dev/repo";

fn store(dir: &std::path::Path) -> McpTrustStore {
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), dir, dir);
    McpTrustStore::new(paths.mcp_trust_store(None))
}

fn key<'a>(server: &'a str, digest: &'a str) -> McpTrustKey<'a> {
    McpTrustKey {
        identity: REPO,
        server,
        digest,
    }
}

async fn same() -> Result<(), McpTrustError> {
    Ok(())
}

async fn text(store: &McpTrustStore) -> String {
    tokio::fs::read_to_string(store.path().as_path())
        .await
        .unwrap()
}

#[tokio::test]
async fn decide_writes_the_node_format_and_keeps_other_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    assert_eq!(store.load().await, McpTrustLoad::Records(Vec::new()));
    assert!(store.decide(&key("evil", A), true, AT, same).await.unwrap());
    assert!(store.decide(&key("evil", B), true, AT, same).await.unwrap());
    // Node 的 `JSON.stringify({schemaVersion: 1, records})`：紧凑，记录的键序同 schema，没有结尾的换行
    let record = |digest: &str| {
        format!(
            r#"{{"workspaceIdentity":"{REPO}","serverName":"evil","declarationDigest":"{digest}","grantedAt":"{AT}"}}"#
        )
    };
    assert_eq!(
        text(&store).await,
        format!(
            r#"{{"schemaVersion":1,"records":[{},{}]}}"#,
            record(A),
            record(B)
        )
    );
    // 撤销只去掉这一份声明（同名的旧 digest 不动，Node 相同）
    assert!(
        store
            .decide(&key("evil", A), false, AT, same)
            .await
            .unwrap()
    );
    assert!(!store.load().await.trusts(&key("evil", A)));
    assert!(store.load().await.trusts(&key("evil", B)));
    let other = McpTrustKey {
        identity: "/Users/dev/other",
        ..key("evil", B)
    };
    assert!(!store.load().await.trusts(&other));
}

#[cfg(unix)]
#[tokio::test]
async fn the_file_and_its_directory_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.decide(&key("evil", A), true, AT, same).await.unwrap();
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let file = store.path().as_path();
    assert_eq!(mode(file), 0o600);
    assert_eq!(mode(file.parent().unwrap()), 0o700);
    // 锁目录用完就删
    assert!(!file.with_extension("json.lock").exists());
}

#[tokio::test]
async fn the_result_already_in_place_is_not_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    assert!(
        !store
            .decide(&key("evil", A), false, AT, same)
            .await
            .unwrap()
    );
    assert!(!store.path().as_path().exists());
    store.decide(&key("evil", A), true, AT, same).await.unwrap();
    let before = text(&store).await;
    let later = "2026-11-06T08:00:00.000Z";
    assert!(
        !store
            .decide(&key("evil", A), true, later, same)
            .await
            .unwrap()
    );
    assert_eq!(text(&store).await, before);
}

#[tokio::test]
async fn a_failed_check_under_the_lock_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let changed = store
        .decide(&key("evil", A), true, AT, async || {
            Err(McpTrustError::DeclarationChanged)
        })
        .await;
    assert!(matches!(changed, Err(McpTrustError::DeclarationChanged)));
    assert!(!store.path().as_path().exists());
}

#[tokio::test]
async fn a_corrupt_store_trusts_nothing_and_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let valid = format!(
        r#"{{"workspaceIdentity":"{REPO}","serverName":"evil","declarationDigest":"{A}","grantedAt":"{AT}"}}"#
    );
    let bad = [
        "{".to_owned(),
        "[]".to_owned(),
        format!(r#"{{"schemaVersion":2,"records":[{valid}]}}"#),
        format!(r#"{{"schemaVersion":1,"records":[{valid}],"extra":true}}"#),
        format!(
            r#"{{"schemaVersion":1,"records":[{{"workspaceIdentity":"{REPO}","serverName":"evil","declarationDigest":"{A}"}}]}}"#
        ),
        format!(
            r#"{{"schemaVersion":1,"records":[{{"workspaceIdentity":"{REPO}","serverName":"evil","declarationDigest":"{}","grantedAt":"{AT}"}}]}}"#,
            A.to_uppercase()
        ),
        format!(
            r#"{{"schemaVersion":1,"records":[{{"workspaceIdentity":"","serverName":"evil","declarationDigest":"{A}","grantedAt":"{AT}"}}]}}"#
        ),
        format!(
            r#"{{"schemaVersion":1,"records":[{{"workspaceIdentity":"{REPO}","serverName":"evil","declarationDigest":"{A}","grantedAt":"yesterday"}}]}}"#
        ),
    ];
    for text in bad {
        write(store.path().as_path(), &text).await;
        assert_eq!(store.load().await, McpTrustLoad::Corrupt, "{text}");
        let refused = store.decide(&key("evil", A), true, AT, same).await;
        assert!(matches!(refused, Err(McpTrustError::Corrupt)), "{text}");
        assert_eq!(
            tokio::fs::read_to_string(store.path().as_path())
                .await
                .unwrap(),
            text
        );
    }
    // `schemaVersion: 1.0` 与 Node 的 `JSON.parse` 一样读成 1
    write(
        store.path().as_path(),
        &format!(r#"{{"schemaVersion":1.0,"records":[{valid}]}}"#),
    )
    .await;
    assert!(store.load().await.trusts(&key("evil", A)));
}
