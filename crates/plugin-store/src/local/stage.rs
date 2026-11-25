//! 暂存（plugin-store.md §5.1、§6.4）：锁外把内容放进 `<父目录>/.<名字>.rust-stage-<pid>-<随机>/`（`content/` 是要提交的
//! 目录，`work/` 是下载、解包、clone 的草稿）；锁内整个改名提交。名字带 pid：崩溃留下的暂存由下一个持锁的写者按 pid 判死后删。
//! Node 不认这个名字，不碰它。

use std::path::{Path, PathBuf};

use crate::core::diagnostic::StoreError;
use crate::core::digest::{self, Item};

/// 暂存目录名里的标记。
const STAGE_MARK: &str = ".rust-stage-";
/// 替换市场目录时旧目录名里的标记（§6.4）。
const OLD_MARK: &str = ".rust-old-";
const CONTENT: &str = "content";
const WORK: &str = "work";

/// 一个暂存容器。
#[derive(Debug)]
pub(crate) struct Stage {
    container: PathBuf,
}

/// `.<base><标记><pid>-<随机>`。
fn scratch_name(base: &str, mark: &str) -> String {
    format!(
        ".{base}{mark}{}-{:x}",
        std::process::id(),
        rust_cli_base::random::u64()
    )
}

/// 暂存容器名 `.<base>.rust-stage-<pid>-<随机>`。
pub(crate) fn stage_name(base: &str) -> String {
    scratch_name(base, STAGE_MARK)
}

/// 市场目录替换时旧目录的名字 `.<base>.rust-old-<pid>-<随机>`。
pub(crate) fn old_name(base: &str) -> String {
    scratch_name(base, OLD_MARK)
}

/// 某个目标的旧目录名的前缀。
pub(crate) fn old_prefix(base: &str) -> String {
    format!(".{base}{OLD_MARK}")
}

/// 名字里的 pid（不是暂存目录时为 None）。
fn stage_pid(name: &str) -> Option<u32> {
    let (_, rest) = name.split_once(STAGE_MARK)?;
    rest.split('-').next()?.parse().ok()
}

impl Stage {
    /// 在 `parent` 下建一个暂存容器（父目录按需建）。
    pub(crate) async fn create(parent: &Path, base: &str) -> Result<Self, StoreError> {
        let container = parent.join(stage_name(base));
        rust_cli_fs::create_dir_all(&container.join(WORK))
            .await
            .map_err(|error| StoreError::io(&container, &error))?;
        Ok(Self { container })
    }

    pub(crate) fn content(&self) -> PathBuf {
        self.container.join(CONTENT)
    }

    pub(crate) fn work(&self) -> PathBuf {
        self.container.join(WORK)
    }

    /// 删掉容器。删不掉只记 warn：留下的容器由下一个持锁的写者按 pid 删（§6.4）。
    pub(crate) async fn discard(self) {
        if let Err(error) = rust_cli_fs::remove_tree(&self.container).await {
            tracing::warn!(dir = %self.container.display(), %error, "a plugin stage could not be removed");
        }
    }
}

/// 删掉 `dir` 下 pid 已经不在的暂存容器（持存储锁时调，§6.4）。`dir` 不存在时什么都不做。
pub(crate) async fn sweep_dead(dir: &Path) {
    let Ok(names) = rust_cli_fs::subdirectories(dir).await else {
        return;
    };
    for name in names {
        let Some(pid) = stage_pid(&name) else {
            continue;
        };
        if pid == std::process::id() || rust_cli_fs::process_alive(pid) {
            continue;
        }
        let path = dir.join(&name);
        match rust_cli_fs::remove_tree(&path).await {
            Ok(()) => {
                tracing::info!(event = "plugins.stage.swept", dir = %path.display(), "removed a dead plugin stage")
            }
            Err(error) => {
                tracing::warn!(dir = %path.display(), %error, "a dead plugin stage could not be removed")
            }
        }
    }
}

/// `root` 这棵树的摘要（标记文件不算，§6.2）。
pub(crate) async fn digest_tree(root: &Path) -> Result<String, StoreError> {
    let entries = rust_cli_fs::walk(root)
        .await
        .map_err(|error| StoreError::io(root, &error))?;
    let mut contents: Vec<(String, Vec<u8>, rust_cli_fs::TreeEntryKind)> = Vec::new();
    for entry in entries {
        let path = entry
            .path
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let bytes = match &entry.kind {
            rust_cli_fs::TreeEntryKind::File => {
                let file = root.join(&entry.path);
                rust_cli_fs::read_optional(&file)
                    .await
                    .map_err(|error| StoreError::io(&file, &error))?
                    .unwrap_or_default()
            }
            rust_cli_fs::TreeEntryKind::Link(target) => {
                target.to_string_lossy().into_owned().into_bytes()
            }
            rust_cli_fs::TreeEntryKind::Dir => Vec::new(),
        };
        contents.push((path, bytes, entry.kind));
    }
    let items: Vec<(String, Item<'_>)> = contents
        .iter()
        .map(|(path, bytes, kind)| {
            let item = match kind {
                rust_cli_fs::TreeEntryKind::Dir => Item::Dir,
                rust_cli_fs::TreeEntryKind::File => Item::File(bytes),
                rust_cli_fs::TreeEntryKind::Link(_) => {
                    Item::Link(std::str::from_utf8(bytes).unwrap_or_default())
                }
            };
            (path.clone(), item)
        })
        .collect();
    Ok(digest::digest(&items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names_carry_the_pid() {
        let name = stage_name("kit");
        assert!(name.starts_with(".kit.rust-stage-"));
        assert_eq!(stage_pid(&name), Some(std::process::id()));
        assert_eq!(stage_pid(".kit.stage-123"), None);
        assert_eq!(stage_pid("1.0.0-abc"), None);
    }

    #[tokio::test]
    async fn dead_stages_are_swept_and_live_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let live = Stage::create(dir.path(), "kit").await.unwrap();
        // 一个不可能活着的 pid（大于 32 位系统上的上限）。
        let dead = dir.path().join(".kit.rust-stage-4294967294-1");
        rust_cli_fs::create_dir_all(&dead).await.unwrap();
        sweep_dead(dir.path()).await;
        assert!(rust_cli_fs::is_dir(&live.work()).await);
        assert!(!rust_cli_fs::is_dir(&dead).await);
        live.discard().await;
    }

    #[tokio::test]
    async fn digests_change_with_content() {
        let dir = tempfile::tempdir().unwrap();
        rust_cli_fs::write_file(&dir.path().join("a/b.txt"), b"1")
            .await
            .unwrap();
        let first = digest_tree(dir.path()).await.unwrap();
        rust_cli_fs::write_file(&dir.path().join(digest::PACKAGE_MARKER), b"x")
            .await
            .unwrap();
        assert_eq!(first, digest_tree(dir.path()).await.unwrap());
        rust_cli_fs::write_file(&dir.path().join("a/b.txt"), b"2")
            .await
            .unwrap();
        assert_ne!(first, digest_tree(dir.path()).await.unwrap());
    }
}
