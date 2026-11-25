//! 委托模式（platform.md §13.13）：值经端口交给别的进程加解密；读只解声明的键，写经端口加密，本进程不取密钥。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::credential_cipher::{CipherFailure, CredentialCipher};

use super::*;
use crate::test_support::{read_text, write};

/// 假加解密方：加密是 `fake:<明文>`；`fail` 时按它失败。记下每一次调用。
#[derive(Default)]
struct FakeCipher {
    calls: Mutex<Vec<String>>,
    fail: Mutex<Option<CipherFailure>>,
}

impl FakeCipher {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn failing(&self, failure: CipherFailure) {
        *self.fail.lock().unwrap() = Some(failure);
    }
}

#[async_trait]
impl CredentialCipher for FakeCipher {
    async fn encrypt(&self, plain: &RedactedString) -> Result<String, CipherFailure> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("encrypt {}", plain.expose()));
        if let Some(failure) = self.fail.lock().unwrap().clone() {
            return Err(failure);
        }
        Ok(format!("fake:{}", plain.expose()))
    }
    async fn decrypt(&self, sealed: &str) -> Result<RedactedString, CipherFailure> {
        self.calls.lock().unwrap().push(format!("decrypt {sealed}"));
        if let Some(failure) = self.fail.lock().unwrap().clone() {
            return Err(failure);
        }
        sealed
            .strip_prefix("fake:")
            .map(RedactedString::new)
            .ok_or(CipherFailure::Decrypt)
    }
}

fn storage(dir: &std::path::Path) -> StoragePath {
    rust_cli_paths::StoragePaths::resolve(&Default::default(), dir, dir).credentials()
}

fn delegated(dir: &std::path::Path) -> (CredentialStore, Arc<FakeCipher>) {
    let cipher = Arc::new(FakeCipher::default());
    (
        CredentialStore::delegated(storage(dir), cipher.clone()),
        cipher,
    )
}

async fn seed(dir: &std::path::Path, record: serde_json::Value) {
    write(storage(dir).as_path(), &record.to_string()).await;
}

#[tokio::test]
async fn reads_only_the_declared_keys_through_the_cipher() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        serde_json::json!({"a": "fake:one", "b": "fake:two", "account": "enc:v2:os:abc:def"}),
    )
    .await;
    let (store, cipher) = delegated(dir.path());
    let read = store
        .read_keys(&["a".to_owned(), "missing".to_owned()])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.get("a").unwrap().unwrap().expose(), "one");
    assert_eq!(read.get("missing").unwrap(), None);
    // 只解了声明且在文件里的键；别的键（账号凭据）不经端口
    assert_eq!(cipher.calls(), vec!["decrypt fake:one"]);
    assert!(matches!(read.get("b"), Err(CredentialError::Delegated(_))));
}

#[tokio::test]
async fn absent_keys_never_reach_the_cipher() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        serde_json::json!({"account": "enc:v2:os:abc:def"}),
    )
    .await;
    let (store, cipher) = delegated(dir.path());
    assert!(store.read_keys(&["a".to_owned()]).await.unwrap().is_none());
    assert!(!store.contains_any(&["a".to_owned()]).await.unwrap());
    assert!(cipher.calls().is_empty());
}

#[tokio::test]
async fn writes_are_sealed_by_the_cipher_and_other_values_are_untouched() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), serde_json::json!({"account": "enc:v1:kept"})).await;
    let (store, cipher) = delegated(dir.path());
    store
        .mutate(|credentials| {
            credentials.set("token", "secret");
            Ok(())
        })
        .await
        .unwrap();
    let text = read_text(storage(dir.path()).as_path()).await.unwrap();
    let record: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        record,
        serde_json::json!({"account": "enc:v1:kept", "token": "fake:secret"})
    );
    assert_eq!(cipher.calls(), vec!["encrypt secret"]);
}

#[tokio::test]
async fn an_unavailable_cipher_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), serde_json::json!({"account": "enc:v1:kept"})).await;
    let (store, cipher) = delegated(dir.path());
    cipher.failing(CipherFailure::Unavailable("link closed".to_owned()));
    let error = store
        .mutate(|credentials| {
            credentials.set("token", "secret");
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(error, CredentialError::Delegated("link closed".to_owned()));
    let text = read_text(storage(dir.path()).as_path()).await.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        serde_json::json!({"account": "enc:v1:kept"})
    );
}

#[tokio::test]
async fn undecryptable_values_read_as_absent() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), serde_json::json!({"a": "garbage"})).await;
    let (store, _) = delegated(dir.path());
    let read = store.read_keys(&["a".to_owned()]).await.unwrap().unwrap();
    assert!(matches!(read.get("a"), Err(CredentialError::Decrypt(_))));
    assert!(read.present("a").is_none());
}

#[tokio::test]
async fn deletes_and_stored_values_need_no_cipher() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        serde_json::json!({"a": "fake:one", "b": "fake:two"}),
    )
    .await;
    let (store, cipher) = delegated(dir.path());
    store
        .mutate(|credentials| {
            assert_eq!(credentials.stored("a"), Some("fake:one"));
            credentials.remove("a");
            Ok(())
        })
        .await
        .unwrap();
    let text = read_text(storage(dir.path()).as_path()).await.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        serde_json::json!({"b": "fake:two"})
    );
    assert!(cipher.calls().is_empty());
}
