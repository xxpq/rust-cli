//! 密钥的来源（platform.md §13.2、§13.5）：受管密钥、固定的来源、缺省的 `credential-secret`、系统凭据库、缓存与并发。

use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::credentials::testing::MemoryKeyStore;
use crate::test_support::{read_text, write};

const SECRET_32: &str = "0123456789abcdef0123456789abcdef";

fn material(explicit: Option<&str>, explicit_file: Option<&Path>) -> CredentialSecret {
    CredentialSecret {
        explicit: explicit.map(RedactedString::new),
        explicit_file: explicit_file.map(Path::to_path_buf),
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    file: PathBuf,
    store: Arc<MemoryKeyStore>,
}

impl Fixture {
    fn new(store: MemoryKeyStore) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".zcode/v2/credentials.json");
        Self {
            _dir: dir,
            file,
            store: Arc::new(store),
        }
    }

    fn source(&self, secret: &CredentialSecret) -> KeySource {
        KeySource::new(&self.file, secret, self.store.clone())
    }

    fn sibling(&self, name: &str) -> PathBuf {
        self.file.with_file_name(name)
    }

    fn stored(&self) -> Option<RedactedString> {
        self.store.get(SERVICE, &account_of(&self.file))
    }

    async fn write_private(&self, name: &str, content: &str) {
        rust_cli_fs::atomic_write(&self.sibling(name), content.as_bytes(), PRIVATE_WRITE)
            .await
            .unwrap();
    }
}

fn is_os(key: &KeyMaterial) -> bool {
    key.encrypt("x").unwrap().starts_with("enc:v2:os:")
}

#[cfg(unix)]
#[test]
fn accounts_follow_the_node_formula() {
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/credential-v2-golden.json"
    ))
    .unwrap();
    assert_eq!(golden["service"], SERVICE);
    for case in golden["accounts"].as_array().unwrap() {
        let path = Path::new(case["path"].as_str().unwrap());
        assert_eq!(account_of(path), case["account"].as_str().unwrap());
    }
}

#[tokio::test]
async fn the_explicit_secret_wins_without_touching_the_store() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    let key = fixture
        .source(&material(Some(" spaced "), None))
        .key(true)
        .await
        .unwrap();
    assert!(!is_os(&key));
    assert_eq!(fixture.store.reads(), 0);
    assert_eq!(read_text(&fixture.sibling(SOURCE_FILE)).await, None);
}

#[tokio::test]
async fn both_managed_sources_together_are_refused() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    fixture.write_private("explicit", SECRET_32).await;
    let secret = material(Some("s"), Some(&fixture.sibling("explicit")));
    assert_eq!(
        fixture.source(&secret).key(true).await.err(),
        Some(CredentialError::Unavailable)
    );
}

#[tokio::test]
async fn the_explicit_secret_file_is_read_as_is_and_checked() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    let path = fixture.sibling("explicit");
    // 内容原样（含换行），作为受管密钥：与环境变量给同一个值的密钥相同
    let content = format!("{SECRET_32}\n");
    fixture.write_private("explicit", &content).await;
    let from_file = fixture
        .source(&material(None, Some(&path)))
        .key(true)
        .await
        .unwrap();
    let from_env = fixture
        .source(&material(Some(&content), None))
        .key(true)
        .await
        .unwrap();
    let sealed = from_file.encrypt("v").unwrap();
    assert_eq!(from_env.decrypt(&sealed).unwrap(), "v");
    for refused in [&SECRET_32[1..], &"x".repeat(4097)] {
        fixture.write_private("explicit", refused).await;
        let key = fixture.source(&material(None, Some(&path))).key(true).await;
        assert_eq!(
            key.err(),
            Some(CredentialError::Unavailable),
            "{} bytes",
            refused.len()
        );
    }
    fixture.write_private("explicit", &"x".repeat(4096)).await;
    assert!(
        fixture
            .source(&material(None, Some(&path)))
            .key(true)
            .await
            .is_ok()
    );
    let missing = fixture.sibling("missing");
    let not_a_file = fixture.file.parent().unwrap().to_path_buf();
    for path in [missing, not_a_file] {
        let key = fixture.source(&material(None, Some(&path))).key(true).await;
        assert_eq!(
            key.err(),
            Some(CredentialError::Unavailable),
            "{}",
            path.display()
        );
    }
    assert_eq!(fixture.store.reads(), 0);
}

#[tokio::test]
async fn the_default_secret_file_is_used_before_the_store() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    fixture.write_private(SECRET_FILE, SECRET_32).await;
    let key = fixture
        .source(&material(None, None))
        .key(true)
        .await
        .unwrap();
    assert!(!is_os(&key));
    assert_eq!(fixture.store.reads(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_shared_default_secret_file_is_refused() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    let options = rust_cli_fs::WriteOptions {
        mode: rust_cli_fs::FileMode::Default,
        ..PRIVATE_WRITE
    };
    rust_cli_fs::atomic_write(&fixture.sibling(SECRET_FILE), SECRET_32.as_bytes(), options)
        .await
        .unwrap();
    let key = fixture.source(&material(None, None)).key(true).await;
    assert_eq!(key.err(), Some(CredentialError::Unavailable));
}

#[tokio::test]
async fn a_pinned_source_or_existing_os_data_ignores_the_default_secret_file() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    fixture.write_private(SECRET_FILE, SECRET_32).await;
    fixture.write_private(SOURCE_FILE, "os\n").await;
    let secret = material(None, None);
    // 固定了系统凭据库、库里又没有密钥：不新建，也不改用文件
    assert_eq!(
        fixture.source(&secret).key(true).await.err(),
        Some(CredentialError::Unavailable)
    );
    fixture.store.put(
        SERVICE,
        &account_of(&fixture.file),
        "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA",
    );
    assert!(is_os(&fixture.source(&secret).key(true).await.unwrap()));

    let other = Fixture::new(MemoryKeyStore::default());
    other.write_private(SECRET_FILE, SECRET_32).await;
    write(
        &other.file,
        r#"{"a":"enc:v2:os:000000000000000000000000:a.b.c"}"#,
    )
    .await;
    assert_eq!(
        other.source(&secret).key(true).await.err(),
        Some(CredentialError::Unavailable)
    );
}

#[tokio::test]
async fn a_pin_that_is_not_os_is_refused() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    fixture.write_private(SOURCE_FILE, "env\n").await;
    let key = fixture.source(&material(None, None)).key(true).await;
    assert_eq!(key.err(), Some(CredentialError::Unavailable));
    assert_eq!(fixture.store.reads(), 0);
}

#[tokio::test]
async fn the_first_writer_creates_the_key_and_pins_the_source() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    let secret = material(None, None);
    // 解 v2 时（不允许新建）库里没有密钥：不可用，不新建
    assert_eq!(
        fixture.source(&secret).key(false).await.err(),
        Some(CredentialError::Unavailable)
    );
    assert_eq!(fixture.store.count(), 0);
    let key = fixture.source(&secret).key(true).await.unwrap();
    assert!(is_os(&key));
    let stored = fixture.stored().unwrap();
    assert_eq!(stored.expose().len(), ENCODED_KEY_CHARS);
    assert_eq!(
        read_text(&fixture.sibling(SOURCE_FILE)).await.as_deref(),
        Some("os\n")
    );
    #[cfg(unix)]
    assert_eq!(
        rust_cli_fs::file_facts(&fixture.sibling(SOURCE_FILE))
            .await
            .unwrap()
            .unwrap()
            .mode,
        Some(rust_cli_base::proto::mode::PRIVATE_FILE)
    );
    // 另一个进程（另一份 KeySource）读到同一把密钥
    let again = fixture.source(&secret).key(false).await.unwrap();
    let sealed = key.encrypt("v").unwrap();
    assert_eq!(again.decrypt(&sealed).unwrap(), "v");
}

#[tokio::test]
async fn existing_v2_data_never_gets_a_replacement_key() {
    let secret = material(None, None);
    for content in [
        r#"{"a":"enc:v2:os:000000000000000000000000:a.b.c"}"#,
        r#"{"a":"enc:v2:env:000000000000000000000000:a.b.c"}"#,
    ] {
        let fixture = Fixture::new(MemoryKeyStore::default());
        write(&fixture.file, content).await;
        let key = fixture.source(&secret).key(true).await;
        assert_eq!(key.err(), Some(CredentialError::Unavailable), "{content}");
        assert_eq!(fixture.store.count(), 0, "{content}");
    }
}

#[tokio::test]
async fn a_stored_value_that_is_not_a_key_is_refused() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    fixture
        .store
        .put(SERVICE, &account_of(&fixture.file), "too-short");
    let key = fixture.source(&material(None, None)).key(true).await;
    assert_eq!(key.err(), Some(CredentialError::Unavailable));
}

#[tokio::test]
async fn failures_are_remembered_and_the_store_is_asked_once() {
    let fixture = Fixture::new(MemoryKeyStore::unavailable());
    let source = fixture.source(&material(None, None));
    for _ in 0..3 {
        assert_eq!(
            source.key(true).await.err(),
            Some(CredentialError::Unavailable)
        );
    }
    assert_eq!(fixture.store.reads(), 1);
}

/// 等 keyring 锁超时（另一个进程持锁等用户授权）没有碰凭据库，不缓存：锁放开之后下一次调用成功。
#[tokio::test]
async fn a_lock_timeout_is_not_remembered() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    let mut lock = fixture.file.clone().into_os_string();
    lock.push(KEYRING_LOCK_SUFFIX);
    let lock = PathBuf::from(lock);
    let source = fixture.source(&material(None, None));
    rust_cli_fs::create_dir_all(fixture.file.parent().unwrap())
        .await
        .unwrap();
    let held = rust_cli_fs::acquire_file_lock(&lock, &LockOptions::default())
        .await
        .unwrap();
    assert_eq!(
        source.key(true).await.err(),
        Some(CredentialError::Unavailable)
    );
    assert_eq!(fixture.store.reads(), 0);
    held.release().await;
    assert!(is_os(&source.key(true).await.unwrap()));
}

/// 首次并发启动（每个任务一份 KeySource，相当于不同的进程）：keyring 锁里只建一把密钥。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_use_creates_one_key() {
    let fixture = Fixture::new(MemoryKeyStore::default());
    let secret = material(None, None);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let source = fixture.source(&secret);
        tasks.push(tokio::spawn(async move {
            source.key(true).await.unwrap().encrypt("v").unwrap()
        }));
    }
    let mut sealed = Vec::new();
    for task in tasks {
        sealed.push(task.await.unwrap());
    }
    let reader = fixture.source(&secret).key(false).await.unwrap();
    for value in sealed {
        assert_eq!(reader.decrypt(&value).unwrap(), "v");
    }
    assert_eq!(fixture.store.count(), 1);
}
