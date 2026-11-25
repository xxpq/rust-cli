//! 存储锁（plugin-store.md §4）：进程内按到达顺序的门 → Node 的目录锁 `installed_plugins.json.lock/`。等锁可以取消；最长等
//! 5 分钟（Node `MANAGEMENT_WAIT_MS`：Node 在锁里下载，可能持很久）。

use std::time::Duration;

use rust_cli_fs::{FileLock, LockOptions};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

use super::LocalStore;
use crate::core::diagnostic::StoreError;

/// 等存储锁的上限（Node `MANAGEMENT_WAIT_MS`）。
const STORAGE_LOCK_WAIT: Duration = Duration::from_secs(5 * 60);

/// 持着存储锁。提交的方法在 `install`、`market`、`orphans` 等模块里；用完 [`Locked::release`]。
pub struct Locked<'a> {
    pub(crate) store: &'a LocalStore,
    lock: FileLock,
    _turn: OwnedSemaphorePermit,
}

impl LocalStore {
    /// 拿存储锁：先排进程内的队，再拿目录锁。取消时返回 `Cancelled`；等到上限返回 `Locked`。
    pub async fn lock(&self, cancel: &CancellationToken) -> Result<Locked<'_>, StoreError> {
        let turn = tokio::select! {
            turn = self.gate.0.clone().acquire_owned() => {
                turn.map_err(|_| StoreError::Invalid("the plugin store gate is closed".to_owned()))?
            }
            () = cancel.cancelled() => return Err(StoreError::Cancelled),
        };
        let options = LockOptions {
            max_wait: STORAGE_LOCK_WAIT,
            cancel: Some(cancel.clone()),
            ..LockOptions::default()
        };
        let file = self.layout.installed();
        rust_cli_fs::create_dir_all(self.layout.root())
            .await
            .map_err(|error| StoreError::io(self.layout.root(), &error))?;
        let lock = match rust_cli_fs::acquire_file_lock(&file, &options).await {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return Err(StoreError::Cancelled);
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                return Err(StoreError::Locked(error.to_string()));
            }
            Err(error) => return Err(StoreError::io(&file, &error)),
        };
        tracing::debug!(event = "plugins.store.locked", root = %self.layout.root().display(), "plugin storage locked");
        Ok(Locked {
            store: self,
            lock,
            _turn: turn,
        })
    }
}

impl Locked<'_> {
    pub async fn release(self) {
        self.lock.release().await;
    }
}
