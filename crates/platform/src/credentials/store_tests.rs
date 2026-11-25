//! 凭据的读、迁移与读改写（platform.md §5.1、§5.5、§13.4）：与 Node 的 `credentialService.ts`、`shared-credentials.ts`
//! 同一份文件、同一种写法。

use std::sync::Arc;

use super::*;
use crate::credentials::testing::MemoryKeyStore;
use crate::test_support::{read_text, write};

fn secret(explicit: Option<&str>) -> CredentialSecret {
    CredentialSecret {
        explicit: explicit.map(RedactedString::new),
        explicit_file: None,
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

/// Node 的 v1 缺省派生（platform linux、home /home/dev、用户 dev）、IV 01..0c 加密 `sk-fixture-国` 的结果。
const NODE_CIPHERTEXT: &str = "enc:v1:AQIDBAUGBwgJCgsM.ugbwcGz6yNOO7MTsaggOWA.TSZzRstDM_2VG1gJJ9g";

fn golden() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/credential-v2-golden.json"
    ))
    .unwrap()
}

fn storage(dir: &std::path::Path) -> StoragePath {
    rust_cli_paths::StoragePaths::resolve(&Default::default(), dir, dir).credentials()
}

/// 系统凭据库里放好 Node 夹具的那把密钥。
fn golden_keys(file: &StoragePath) -> Arc<MemoryKeyStore> {
    let keys = Arc::new(MemoryKeyStore::default());
    keys.put(
        keys::SERVICE,
        &keys::account_of(file.as_path()),
        golden()["osKey"].as_str().unwrap(),
    );
    keys
}

fn store(dir: &std::path::Path, explicit: Option<&str>) -> (CredentialStore, Arc<MemoryKeyStore>) {
    let file = storage(dir);
    let keys = Arc::new(MemoryKeyStore::default());
    (
        CredentialStore::new(file, &secret(explicit), keys.clone()),
        keys,
    )
}

async fn record(file: &std::path::Path) -> Map<String, Value> {
    serde_json::from_str(&read_text(file).await.unwrap()).unwrap()
}

/// CRED-V2 在旧代码上失败的那条：Node 用受管密钥写的 `enc:v2:env:` 值解成明文（旧代码把它当明文原样交出）。
#[tokio::test]
async fn node_v2_env_values_are_decrypted() {
    let golden = golden();
    let dir = tempfile::tempdir().unwrap();
    let (store, keys) = store(dir.path(), golden["envSecret"].as_str());
    let value = &golden["values"][0];
    write(
        store.file().as_path(),
        serde_json::json!({ "a": value["env"] }).to_string(),
    )
    .await;
    let credentials = store.read().await.unwrap();
    assert_eq!(
        credentials.get("a").unwrap().map(|v| v.expose().to_owned()),
        value["plain"].as_str().map(str::to_owned)
    );
    assert_eq!(keys.reads(), 0);
}

#[tokio::test]
async fn node_v2_os_values_are_decrypted_with_the_system_key() {
    let golden = golden();
    let dir = tempfile::tempdir().unwrap();
    let file = storage(dir.path());
    let keys = golden_keys(&file);
    let store = CredentialStore::new(file, &secret(None), keys.clone());
    let mut written = Map::new();
    for (index, value) in golden["values"].as_array().unwrap().iter().enumerate() {
        written.insert(format!("k{index}"), value["os"].clone());
    }
    write(store.file().as_path(), Value::Object(written).to_string()).await;
    let credentials = store.read().await.unwrap();
    for (index, value) in golden["values"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            credentials
                .get(&format!("k{index}"))
                .unwrap()
                .map(|v| v.expose().to_owned()),
            value["plain"].as_str().map(str::to_owned)
        );
    }
    // 读不新建、不改文件：已有 v2 的文件只读密钥
    assert_eq!(keys.count(), 1);
    assert_eq!(
        read_text(&store.file().as_path().with_file_name("credential-source"))
            .await
            .as_deref(),
        Some("os\n")
    );
}

#[tokio::test]
async fn records_are_read_from_disk_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let (store, keys) = store(dir.path(), None);
    assert_eq!(store.read().await.unwrap().get("a").unwrap(), None);
    // 空文件不碰凭据库
    assert_eq!(keys.reads(), 0);
    let file = store.file().as_path().to_path_buf();
    write(
        &file,
        format!(r#"{{"a":"{NODE_CIPHERTEXT}","legacy":"plain","blank":"  ","bad":"enc:v1:x"}}"#),
    )
    .await;
    let credentials = store.read().await.unwrap();
    assert_eq!(credentials.present("a").unwrap().expose(), "sk-fixture-国");
    assert_eq!(credentials.present("legacy").unwrap().expose(), "plain");
    assert!(credentials.present("blank").is_none());
    // 坏密文按“没有”处理，不当作已登录。
    assert!(credentials.present("bad").is_none());
    assert!(credentials.get("bad").is_err());

    write(&file, r#"{"a": 1}"#).await;
    assert!(matches!(
        store.read().await,
        Err(CredentialError::Corrupt(_))
    ));
}

/// 用户决定（2026-11-11）：rust 不迁移。v1 文件读过之后一字不变，不建密钥、不固定来源。
#[tokio::test]
async fn v1_files_are_not_migrated_and_broken_values_stay() {
    let dir = tempfile::tempdir().unwrap();
    let (store, keys) = store(dir.path(), None);
    let file = store.file().as_path().to_path_buf();
    let text = format!(
        r#"{{"a":"{NODE_CIPHERTEXT}","legacy":"plain","bad":"enc:v1:x","odd":"enc:v9:x"}}"#
    );
    write(&file, &text).await;
    let credentials = store.read().await.unwrap();
    assert_eq!(credentials.present("a").unwrap().expose(), "sk-fixture-国");
    assert_eq!(credentials.present("legacy").unwrap().expose(), "plain");
    assert!(credentials.get("bad").is_err());
    assert!(credentials.get("odd").is_err());
    assert_eq!(read_text(&file).await.unwrap(), text);
    assert_eq!(
        read_text(&file.with_file_name("credential-source")).await,
        None
    );
    assert_eq!(keys.reads(), 0);
}

/// v2 文件里留着的旧值照读、不迁移；别的密钥写的 v2 值是存储错误（按值报出），读本身照常。
#[tokio::test]
async fn v2_files_keep_their_old_values_and_report_other_keys() {
    let golden = golden();
    let dir = tempfile::tempdir().unwrap();
    let (store, _keys) = store(dir.path(), Some("another secret"));
    let file = store.file().as_path().to_path_buf();
    let text = serde_json::json!({ "v2": golden["values"][0]["env"], "old": "plain" }).to_string();
    write(&file, &text).await;
    let credentials = store.read().await.unwrap();
    assert_eq!(
        credentials.get("v2").err(),
        Some(CredentialError::Unavailable)
    );
    assert_eq!(credentials.present("old").unwrap().expose(), "plain");
    assert_eq!(read_text(&file).await.unwrap(), text);
    store
        .mutate(|credentials| {
            credentials.set("new", "x");
            Ok(())
        })
        .await
        .unwrap();
    let written = record(&file).await;
    assert_eq!(written["old"], "plain");
    assert!(written["new"].as_str().unwrap().starts_with("enc:v2:env:"));
}

#[tokio::test]
async fn an_unavailable_store_fails_the_read_but_not_an_empty_file() {
    let golden = golden();
    let dir = tempfile::tempdir().unwrap();
    let file = storage(dir.path());
    let keys = Arc::new(MemoryKeyStore::unavailable());
    let store = CredentialStore::new(file, &secret(None), keys.clone());
    assert!(store.read().await.unwrap().get("a").unwrap().is_none());
    assert_eq!(
        store.writable().await.err(),
        Some(CredentialError::Unavailable)
    );
    let text = serde_json::json!({ "a": golden["values"][0]["os"] }).to_string();
    write(store.file().as_path(), &text).await;
    // 存储错误不是登录失效：不删、不改文件
    assert_eq!(store.read().await.err(), Some(CredentialError::Unavailable));
    let written = store.mutate(|credentials| {
        credentials.set("b", "x");
        Ok(())
    });
    assert_eq!(written.await.err(), Some(CredentialError::Unavailable));
    assert_eq!(read_text(store.file().as_path()).await.unwrap(), text);
    // 失败只问一次凭据库
    assert_eq!(keys.reads(), 1);
}

#[tokio::test]
async fn writes_are_v2_pretty_printed_and_private() {
    let golden = golden();
    let dir = tempfile::tempdir().unwrap();
    let file_path = storage(dir.path());
    let store = CredentialStore::new(file_path.clone(), &secret(None), golden_keys(&file_path));
    let file = store.file().as_path().to_path_buf();
    let keep = golden["values"][0]["os"].as_str().unwrap();
    write(&file, format!(r#"{{"keep":"{keep}","drop":"x"}}"#)).await;
    store
        .mutate(|credentials| {
            credentials.set("oauth:zai:access_token", "token-1");
            credentials.set("second", "token-2");
            credentials.remove("drop");
            // 同一次改动里写了再读，读到的是新值
            assert_eq!(credentials.present("second").unwrap().expose(), "token-2");
            Ok(())
        })
        .await
        .unwrap();
    let text = read_text(&file).await.unwrap();
    // Node `JSON.stringify(value, null, 2) + "\n"`：键序保持，新键按写入的顺序在末尾；已有的值不动。
    let record: Map<String, Value> = serde_json::from_str(&text).unwrap();
    let keys: Vec<&String> = record.keys().collect();
    assert_eq!(keys, ["keep", "oauth:zai:access_token", "second"]);
    let encrypted = record["oauth:zai:access_token"].as_str().unwrap();
    assert!(encrypted.starts_with("enc:v2:os:") && !encrypted.contains("token-1"));
    assert_eq!(record["keep"], keep);
    assert_eq!(
        text,
        format!(
            "{{\n  \"keep\": \"{keep}\",\n  \"oauth:zai:access_token\": \"{encrypted}\",\n  \"second\": \"{}\"\n}}\n",
            record["second"].as_str().unwrap()
        )
    );
    let credentials = store.read().await.unwrap();
    assert_eq!(
        credentials
            .present("oauth:zai:access_token")
            .unwrap()
            .expose(),
        "token-1"
    );
    assert_eq!(
        credentials.present("keep").map(|v| v.expose().to_owned()),
        golden["values"][0]["plain"].as_str().map(str::to_owned)
    );
    #[cfg(unix)]
    #[expect(clippy::disallowed_methods, reason = "测试检查文件权限")]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let lock = dir.path().join(".zcode/v2/credentials.json.lock");
    // 锁目录已释放（还在的话读目录会报错）。
    assert!(matches!(rust_cli_fs::read_optional(&lock).await, Ok(None)));
}

#[tokio::test]
async fn corrupt_files_and_failed_changes_are_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _keys) = store(dir.path(), None);
    let file = store.file().as_path().to_path_buf();
    write(&file, "not json").await;
    let corrupt = store
        .mutate(|credentials| {
            credentials.set("a", "b");
            Ok(())
        })
        .await;
    assert!(matches!(corrupt, Err(CredentialError::Corrupt(_))));
    assert_eq!(read_text(&file).await.unwrap(), "not json");

    // 改动失败时什么都不写
    write(&file, r#"{"old":"plain"}"#).await;
    let failed: Result<(), CredentialError> = store
        .mutate(|credentials| {
            credentials.set("a", "b");
            Err(CredentialError::Write("refused".to_owned()))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(read_text(&file).await.unwrap(), r#"{"old":"plain"}"#);
}

/// 同一进程里并发的读改写经目录锁互斥：每个任务写的键都在。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_do_not_lose_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (store, keys) = store(dir.path(), None);
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .mutate(|credentials| {
                    credentials.set(&format!("key-{index}"), "v");
                    Ok(())
                })
                .await
                .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let credentials = store.read().await.unwrap();
    for index in 0..8 {
        assert!(credentials.present(&format!("key-{index}")).is_some());
    }
    assert_eq!(keys.count(), 1);
}

/// 用户决定（2026-11-11，platform.md §13.4）：只有 v1 与明文的文件不迁移，读写都照 v1，不碰系统凭据库。
#[tokio::test]
async fn v1_files_keep_v1_and_need_no_system_store() {
    let dir = tempfile::tempdir().unwrap();
    let file = storage(dir.path());
    let keys = Arc::new(MemoryKeyStore::unavailable());
    let store = CredentialStore::new(file, &secret(None), keys.clone());
    let path = store.file().as_path().to_path_buf();
    write(
        &path,
        format!(r#"{{"a":"{NODE_CIPHERTEXT}","legacy":"plain"}}"#),
    )
    .await;
    let credentials = store.read().await.unwrap();
    assert_eq!(credentials.present("a").unwrap().expose(), "sk-fixture-国");
    store
        .mutate(|credentials| {
            credentials.set("b", "token-b");
            Ok(())
        })
        .await
        .unwrap();
    let written = record(&path).await;
    assert_eq!(written["a"], NODE_CIPHERTEXT);
    assert_eq!(written["legacy"], "plain");
    assert!(written["b"].as_str().unwrap().starts_with("enc:v1:"));
    assert_eq!(
        store.read().await.unwrap().present("b").unwrap().expose(),
        "token-b"
    );
    assert!(store.writable().await.is_ok());
    assert_eq!(keys.reads(), 0);
}

/// 用户决定（2026-11-11，platform.md §13.4）：凭据库不可用时只删除的读改写照 Node 的纯删除照常提交。
#[tokio::test]
async fn removals_commit_when_the_store_is_unavailable() {
    let golden = golden();
    let dir = tempfile::tempdir().unwrap();
    let file = storage(dir.path());
    let store = CredentialStore::new(file, &secret(None), Arc::new(MemoryKeyStore::unavailable()));
    let path = store.file().as_path().to_path_buf();
    let os = &golden["values"][0]["os"];
    write(
        &path,
        serde_json::json!({ "drop": os, "keep": os }).to_string(),
    )
    .await;
    store
        .mutate(|credentials| {
            assert!(credentials.present("drop").is_none());
            credentials.remove("drop");
            Ok(())
        })
        .await
        .unwrap();
    let written = record(&path).await;
    assert_eq!(written.keys().collect::<Vec<_>>(), ["keep"]);
}
