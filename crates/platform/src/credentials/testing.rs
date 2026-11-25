//! 系统凭据库的测试替身（docs/specs/rust-cli/platform.md §13.8）：内存里的条目，可以设成不可用，记下读了几次。
//! platform 自己的测试与 cli 的进程内测试用（`testing` 特性）。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::sync::LockUnpoisoned;

use super::keys::SystemKeyStore;

/// 内存里的系统凭据库。
#[derive(Default)]
pub struct MemoryKeyStore {
    entries: Mutex<BTreeMap<(String, String), RedactedString>>,
    unavailable: bool,
    reads: AtomicUsize,
}

impl MemoryKeyStore {
    /// 每次读写都报错的凭据库（锁着、拒绝授权、没有 Secret Service）。
    pub fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::default()
        }
    }

    /// 读过几次。
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// 一个条目的值。
    pub fn get(&self, service: &str, account: &str) -> Option<RedactedString> {
        let entries = self.entries.lock_unpoisoned();
        entries
            .get(&(service.to_owned(), account.to_owned()))
            .cloned()
    }

    /// 条目的个数。
    pub fn count(&self) -> usize {
        self.entries.lock_unpoisoned().len()
    }

    /// 放进一个条目（测试准备别的程序已经建好的密钥）。
    pub fn put(&self, service: &str, account: &str, value: &str) {
        let key = (service.to_owned(), account.to_owned());
        self.entries
            .lock_unpoisoned()
            .insert(key, RedactedString::new(value));
    }
}

#[async_trait]
impl SystemKeyStore for MemoryKeyStore {
    async fn read(&self, service: &str, account: &str) -> Result<Option<RedactedString>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.unavailable {
            return Err("the test store is unavailable".to_owned());
        }
        Ok(self.get(service, account))
    }

    async fn write(
        &self,
        service: &str,
        account: &str,
        value: &RedactedString,
    ) -> Result<(), String> {
        if self.unavailable {
            return Err("the test store is unavailable".to_owned());
        }
        self.put(service, account, value.expose());
        Ok(())
    }
}
