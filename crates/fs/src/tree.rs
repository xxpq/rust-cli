//! 目录树：复制、改名、遍历与写入单个文件（插件的暂存与提交，docs/specs/rust-cli/plugin-store.md §5、§6）。
//!
//! 插件包从市场的目录、解开的 zip 或 clone 下来的仓库复制进暂存目录，再整个改名成不可变的安装目录。复制保留文件的权限位
//! （插件里的脚本要能执行；Node `cp` 相同）；符号链接照原样建成链接（Node `cp` 的缺省，不跟随）。

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

/// 树里一项的种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEntryKind {
    File,
    Dir,
    /// 符号链接与它的目标（原样的文字，不解析）。
    Link(PathBuf),
}

/// [`walk`] 的一项：相对根的路径（按段，平台的分隔符）与种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub kind: TreeEntryKind,
}

/// 改名（文件或目录）。目标是目录时它必须不存在（各平台的 rename 对已存在的目录行为不同，调用方先确认）。
pub async fn rename(from: &Path, to: &Path) -> io::Result<()> {
    tokio::fs::rename(from, to).await
}

/// 写一个文件（父目录按需建）；不是原子写，只给还没有人读的暂存目录用。
pub async fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, bytes).await
}

/// `root` 下的全部项（不含 `root` 自己），lstat 遍历、不跟随链接，按相对路径排序。`root` 不存在时报 `NotFound`。
pub async fn walk(root: &Path) -> io::Result<Vec<TreeEntry>> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let mut entries = tokio::fs::read_dir(root.join(&relative)).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = relative.join(entry.file_name());
            let kind = entry.file_type().await?;
            let kind = if kind.is_symlink() {
                TreeEntryKind::Link(tokio::fs::read_link(entry.path()).await?)
            } else if kind.is_dir() {
                pending.push(path.clone());
                TreeEntryKind::Dir
            } else {
                TreeEntryKind::File
            };
            found.push(TreeEntry { path, kind });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// 把 `from` 整个复制成 `to`（`to` 必须不存在）。每复制一项之前看 `cancel`，触发后报 `Interrupted`，已复制的部分留给
/// 调用方删（它在暂存目录里）。
pub async fn copy_tree(from: &Path, to: &Path, cancel: &CancellationToken) -> io::Result<()> {
    let entries = walk(from).await?;
    tokio::fs::create_dir(to).await?;
    for entry in entries {
        if cancel.is_cancelled() {
            return Err(io::Error::new(
                ErrorKind::Interrupted,
                "the copy was cancelled",
            ));
        }
        let target = to.join(&entry.path);
        match entry.kind {
            TreeEntryKind::Dir => tokio::fs::create_dir(&target).await?,
            TreeEntryKind::File => {
                tokio::fs::copy(from.join(&entry.path), &target).await?;
            }
            TreeEntryKind::Link(link) => symlink(&link, &target, &from.join(&entry.path)).await?,
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn symlink(link: &Path, target: &Path, _source: &Path) -> io::Result<()> {
    tokio::fs::symlink(link, target).await
}

/// Windows 的链接分文件与目录两种，按源链接指向的东西选；指向不存在的目标时按文件建。
#[cfg(windows)]
async fn symlink(link: &Path, target: &Path, source: &Path) -> io::Result<()> {
    if tokio::fs::metadata(source)
        .await
        .is_ok_and(|meta| meta.is_dir())
    {
        tokio::fs::symlink_dir(link, target).await
    } else {
        tokio::fs::symlink_file(link, target).await
    }
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
