//! 孤儿目录（plugin-store.md §6.3）：旧安装目录打 `.orphaned_at`（毫秒时间戳），14 天后删，跳过调用方给的
//! 「正在用」的目录。只给 rust 建的目录（有摘要标记）补标记：Node 播种的内置插件目录不在记录里，补了会在 14 天后被删。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::Locked;
use crate::core::ORPHAN_TTL_MS;
use crate::core::diagnostic::StoreError;
use crate::core::digest::PACKAGE_MARKER;

/// 孤儿标记的文件名（两种插件格式共用这个名字，plugin-store.md §6.3）。
pub(crate) const ORPHAN_MARKER: &str = ".orphaned_at";

/// 给 `dir` 打标记（已有就不改，保留最早的时间）。
pub(crate) async fn mark(dir: &Path, now_ms: u64) -> Result<(), StoreError> {
    let marker = dir.join(ORPHAN_MARKER);
    if !rust_cli_fs::is_dir(dir).await || rust_cli_fs::is_file(&marker).await {
        return Ok(());
    }
    rust_cli_fs::write_file(&marker, now_ms.to_string().as_bytes())
        .await
        .map_err(|error| StoreError::io(&marker, &error))
}

/// 去掉 `dir` 的标记（又被引用了）。
pub(crate) async fn unmark(dir: &Path) -> Result<(), StoreError> {
    let marker = dir.join(ORPHAN_MARKER);
    rust_cli_fs::remove_optional(&marker)
        .await
        .map_err(|error| StoreError::io(&marker, &error))
}

async fn marked_at(dir: &Path) -> Option<u64> {
    let bytes = rust_cli_fs::read_optional(&dir.join(ORPHAN_MARKER))
        .await
        .ok()
        .flatten()?;
    String::from_utf8_lossy(&bytes).trim().parse().ok()
}

/// `cache/<mkt>/<name>/<目录>` 的全部版本目录（跳过 `.` 开头的：Node 的备份、暂存与 rust 的暂存）。
async fn version_dirs(cache: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for marketplace in rust_cli_fs::subdirectories(cache).await.unwrap_or_default() {
        let marketplace = cache.join(marketplace);
        for name in rust_cli_fs::subdirectories(&marketplace)
            .await
            .unwrap_or_default()
        {
            let plugin = marketplace.join(name);
            for version in rust_cli_fs::subdirectories(&plugin)
                .await
                .unwrap_or_default()
            {
                if !version.starts_with('.') {
                    found.push(plugin.join(version));
                }
            }
        }
    }
    found
}

impl Locked<'_> {
    /// 清理（§6.3 第二行）：被记录引用的去标记；没被引用、有摘要标记、没有孤儿标记的补标记；有孤儿标记、超过 14 天、不在
    /// `in_use` 里的删。返回删掉的目录。
    pub async fn sweep_orphans(
        &self,
        now_ms: u64,
        in_use: &BTreeSet<PathBuf>,
    ) -> Result<Vec<PathBuf>, StoreError> {
        let installed = self.installed().await?;
        let mut removed = Vec::new();
        for dir in version_dirs(&self.store.layout().cache()).await {
            let path = dir.to_string_lossy();
            if installed.references(&path) {
                unmark(&dir).await?;
                continue;
            }
            match marked_at(&dir).await {
                Some(at) if now_ms.saturating_sub(at) > ORPHAN_TTL_MS && !in_use.contains(&dir) => {
                    rust_cli_fs::remove_tree(&dir)
                        .await
                        .map_err(|error| StoreError::io(&dir, &error))?;
                    tracing::info!(event = "plugins.orphan.removed", dir = %dir.display(), "removed an expired plugin directory");
                    removed.push(dir);
                }
                Some(_) => {}
                None if rust_cli_fs::is_file(&dir.join(PACKAGE_MARKER)).await => {
                    mark(&dir, now_ms).await?
                }
                None => {}
            }
        }
        Ok(removed)
    }

    /// 更新、卸载之后，旧目录在缓存之下、不再被引用时打标记（§6.3 第一行）。
    pub(crate) async fn orphan(
        &self,
        old: &str,
        installed: &crate::core::records::Installed,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        let path = Path::new(old);
        if installed.references(old) || !path.starts_with(self.store.layout().cache()) {
            return Ok(());
        }
        mark(path, now_ms).await
    }
}
