//! 平台层的系统凭据库端口（docs/specs/rust-cli/platform.md §13.8）：产品路径经 keyring crate 访问本机的凭据库；debug
//! 构建设了 `_ZCODE_ASSUME_KEYRING_FILE` 时换成那个 JSON 文件里的假凭据库（node-interop 让 Node 与本工程经同一个文件互读）。
//! macOS 的交互界面另带 [`KeychainNotice`]：本进程第一次访问钥匙串之前提示一句（platform.md §13.12）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_platform::SystemKeyStore;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::process::output::eprint_raw;

/// 进程用的系统凭据库：给了假凭据库的文件就用它，否则是本机的凭据库（`notice`：访问之前的提示，只有 macOS 的交互界面给）。
pub(crate) fn system_keys(
    keyring_file: Option<&PathBuf>,
    notice: Option<Arc<KeychainNotice>>,
) -> Arc<dyn SystemKeyStore> {
    match keyring_file {
        Some(file) => Arc::new(FileKeyStore(file.clone())),
        None => Arc::new(OsKeyStore { notice }),
    }
}

/// 访问钥匙串之前的提示（platform.md §13.12）：本进程只提示一次，提示到哪里看界面走到了哪一步。
#[derive(Default)]
pub(crate) struct KeychainNotice {
    state: Mutex<NoticeState>,
}

enum NoticeState {
    /// 还不能提示（文字没定、或启动画面占着终端）：访问记成待提示。
    Held {
        pending: bool,
    },
    /// 组装平台层之后、启动画面之前：写一行到终端。
    Terminal(&'static str),
    /// 界面在跑：发一个信号，界面在 footer 提示。
    Ui(mpsc::UnboundedSender<()>),
    Shown,
}

impl Default for NoticeState {
    fn default() -> Self {
        Self::Held { pending: false }
    }
}

impl KeychainNotice {
    /// 访问钥匙串之前调用。
    fn before_access(&self) {
        let mut state = self.state.lock_unpoisoned();
        match &*state {
            NoticeState::Held { .. } => *state = NoticeState::Held { pending: true },
            NoticeState::Terminal(text) => {
                eprint_raw(&format!("{text}\n"));
                *state = NoticeState::Shown;
            }
            NoticeState::Ui(sender) => {
                // 界面已经退出时收不到：不用再提示
                let _ = sender.send(());
                *state = NoticeState::Shown;
            }
            NoticeState::Shown => {}
        }
    }

    /// 文字定下了、终端可以写（启动画面之前）。之前记下的待提示现在写出。
    pub(crate) fn terminal(&self, text: &'static str) {
        let mut state = self.state.lock_unpoisoned();
        match &*state {
            NoticeState::Held { pending: true } => {
                eprint_raw(&format!("{text}\n"));
                *state = NoticeState::Shown;
            }
            NoticeState::Held { pending: false } => *state = NoticeState::Terminal(text),
            NoticeState::Terminal(_) | NoticeState::Ui(_) | NoticeState::Shown => {}
        }
    }

    /// 启动画面要占终端了：之后的访问先记下，等界面开始。
    pub(crate) fn hold(&self) {
        let mut state = self.state.lock_unpoisoned();
        if matches!(&*state, NoticeState::Terminal(_)) {
            *state = NoticeState::Held { pending: false };
        }
    }
}

impl rust_cli_tui::KeychainNotices for KeychainNotice {
    /// 界面开始：之后的提示经这个流发给界面；启动画面期间记下的待提示马上发出。
    fn notices(&self) -> BoxStream<'static, ()> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.state.lock_unpoisoned();
        match &*state {
            NoticeState::Held { pending: true } => {
                let _ = sender.send(());
                *state = NoticeState::Shown;
            }
            NoticeState::Held { pending: false } | NoticeState::Terminal(_) => {
                *state = NoticeState::Ui(sender);
            }
            NoticeState::Ui(_) | NoticeState::Shown => {}
        }
        // 提示过之后发送端就丢了，信号流到头；接一个永不出项的流，界面不用处理结束
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|()| ((), receiver))
        })
        .chain(futures_util::stream::pending())
        .boxed()
    }
}

/// 本机的系统凭据库（macOS 钥匙串、Windows 凭据管理器、Linux Secret Service）。
struct OsKeyStore {
    notice: Option<Arc<KeychainNotice>>,
}

impl OsKeyStore {
    fn before_access(&self) {
        if let Some(notice) = &self.notice {
            notice.before_access();
        }
    }
}

#[async_trait]
impl SystemKeyStore for OsKeyStore {
    async fn read(&self, service: &str, account: &str) -> Result<Option<RedactedString>, String> {
        self.before_access();
        rust_cli_keyring::read(service, account)
            .await
            .map(|value| value.map(RedactedString::new))
            .map_err(|error| error.to_string())
    }

    async fn write(
        &self,
        service: &str,
        account: &str,
        value: &RedactedString,
    ) -> Result<(), String> {
        self.before_access();
        rust_cli_keyring::write(service, account, value.expose())
            .await
            .map_err(|error| error.to_string())
    }
}

/// 只给测试的假凭据库：`{ "<服务名>": { "<账号名>": "<值>" } }`。读写都在平台层的 keyring 锁里，不另加锁。
struct FileKeyStore(PathBuf);

/// 假凭据库的写法：只有本用户能读写，同目录隐藏临时文件。
const FILE_WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Private,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: rust_cli_fs::NODE_RENAME_RETRY,
};

impl FileKeyStore {
    async fn entries(&self) -> Result<Map<String, Value>, String> {
        let bytes = rust_cli_fs::read_optional(&self.0)
            .await
            .map_err(|error| error.to_string())?;
        match bytes {
            None => Ok(Map::new()),
            Some(bytes) => match serde_json::from_slice(&bytes) {
                Ok(Value::Object(entries)) => Ok(entries),
                _ => Err(format!("{} is not a JSON object", self.0.display())),
            },
        }
    }
}

#[async_trait]
impl SystemKeyStore for FileKeyStore {
    async fn read(&self, service: &str, account: &str) -> Result<Option<RedactedString>, String> {
        let entries = self.entries().await?;
        let value = entries
            .get(service)
            .and_then(|accounts| accounts.get(account));
        Ok(value.and_then(Value::as_str).map(RedactedString::new))
    }

    async fn write(
        &self,
        service: &str,
        account: &str,
        value: &RedactedString,
    ) -> Result<(), String> {
        let mut entries = self.entries().await?;
        let accounts = entries
            .entry(service)
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(accounts) = accounts {
            accounts.insert(account.to_owned(), Value::String(value.expose().to_owned()));
        }
        let text = Value::Object(entries).to_string();
        rust_cli_fs::atomic_write(&self.0, text.as_bytes(), FILE_WRITE)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
