//! Node 死事务的四种收尾（plugin-store.md §7）。

use serde_json::json;

use super::*;

/// 一个不可能活着的 pid。
const DEAD: u32 = u32::MAX - 1;

struct Fixture {
    _dir: tempfile::TempDir,
    target: PathBuf,
    backup: PathBuf,
    transaction: PathBuf,
    authority: PathBuf,
    stage: PathBuf,
}

async fn fixture(target_exists: bool, backup_exists: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("cache/mkt/kit");
    let target = parent.join("1.0.0");
    let backup = parent.join(".1.0.0.backup");
    if target_exists {
        rust_cli_fs::write_file(&target.join("new.txt"), b"new")
            .await
            .unwrap();
    }
    if backup_exists {
        rust_cli_fs::write_file(&backup.join("old.txt"), b"old")
            .await
            .unwrap();
    }
    let stage = parent.join(".1.0.0.stage-abc");
    rust_cli_fs::create_dir_all(&stage).await.unwrap();
    Fixture {
        transaction: parent.join(".1.0.0.transaction.json"),
        authority: dir.path().join("installed_plugins.json"),
        _dir: dir,
        target,
        backup,
        stage,
    }
}

async fn write_transaction(fixture: &Fixture, pid: u32, had_target: bool) {
    let record = json!({
        "authorityPath": fixture.authority, "hadTarget": had_target, "mode": "coordinated",
        "ownerId": "node", "ownerPid": pid, "stageName": ".1.0.0.stage-abc", "transactionId": "tx-1", "version": 2
    });
    rust_cli_fs::write_file(&fixture.transaction, record.to_string().as_bytes())
        .await
        .unwrap();
}

async fn authority(fixture: &Fixture, id: &str) {
    let doc = json!({"version": 1, "plugins": [{"id": "kit@mkt", "cacheTransactionId": id}]});
    rust_cli_fs::write_file(&fixture.authority, doc.to_string().as_bytes())
        .await
        .unwrap();
}

#[tokio::test]
async fn committed_transactions_keep_the_new_target() {
    let f = fixture(true, true).await;
    write_transaction(&f, DEAD, true).await;
    authority(&f, "tx-1").await;
    settle(&f.target).await.unwrap();
    assert!(rust_cli_fs::is_file(&f.target.join("new.txt")).await);
    assert!(!rust_cli_fs::is_dir(&f.backup).await);
    assert!(!rust_cli_fs::is_dir(&f.stage).await);
    assert!(!rust_cli_fs::is_file(&f.transaction).await);
}

#[tokio::test]
async fn uncommitted_transactions_roll_back_to_the_backup() {
    let f = fixture(true, true).await;
    write_transaction(&f, DEAD, true).await;
    authority(&f, "other").await;
    settle(&f.target).await.unwrap();
    assert!(rust_cli_fs::is_file(&f.target.join("old.txt")).await);
    assert!(!rust_cli_fs::is_file(&f.target.join("new.txt")).await);
    assert!(!rust_cli_fs::is_dir(&f.backup).await);
}

#[tokio::test]
async fn uncommitted_fresh_targets_are_removed() {
    let f = fixture(true, false).await;
    write_transaction(&f, DEAD, false).await;
    authority(&f, "other").await;
    settle(&f.target).await.unwrap();
    assert!(!rust_cli_fs::is_dir(&f.target).await);
}

/// 活着、又不是本进程的 owner（pid 1 的 init / launchd 总在）：不动它的备份与事务文件。
#[cfg(unix)]
#[tokio::test]
async fn live_owners_are_left_alone() {
    let f = fixture(true, true).await;
    write_transaction(&f, 1, true).await;
    settle(&f.target).await.unwrap();
    assert!(rust_cli_fs::is_dir(&f.backup).await);
    assert!(rust_cli_fs::is_file(&f.transaction).await);
}

#[tokio::test]
async fn clean_targets_are_left_alone() {
    let clean = fixture(true, false).await;
    settle(&clean.target).await.unwrap();
    assert!(rust_cli_fs::is_file(&clean.target.join("new.txt")).await);
}

#[tokio::test]
async fn standalone_leftovers_restore_a_missing_target() {
    let f = fixture(false, true).await;
    settle(&f.target).await.unwrap();
    assert!(rust_cli_fs::is_file(&f.target.join("old.txt")).await);
}
