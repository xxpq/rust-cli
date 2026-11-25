//! zip 的解包（plugin-store.md §9.4，Node `zip-source.ts`）：用 `zip` crate 读（逐项读进内存），写文件经 fs。上限照 Node；
//! 拒绝不安全的路径、加密的项、链接与其他非常规项。

use std::collections::BTreeSet;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::core::diagnostic::StoreError;
use crate::core::manifest::MANIFEST_PATHS;

/// 解开的总量、条目数与单个文件的上限（Node `ZIP_EXTRACT_MAX_BYTES`、`ZIP_MAX_ENTRIES`、`ZIP_MAX_SINGLE_FILE_BYTES`）。
const EXTRACT_MAX_BYTES: u64 = 500 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// unix 模式里的文件类型位与几种类型（Node `classifyZipEntry`）。
const TYPE_MASK: u32 = 0o170_000;
const TYPE_LINK: u32 = 0o120_000;
const TYPE_FILE: u32 = 0o100_000;
const TYPE_DIR: u32 = 0o040_000;
/// 盘符 `X:` 的字节数。
const DRIVE_LEN: usize = 2;

/// 一项解出的结果。
enum Item {
    Dir(String),
    File(String, Vec<u8>),
}

/// 解开的结果：顶层的名字（`stripRoot`、Archive 的「只有一个顶层目录」用）。
#[derive(Debug)]
pub(crate) struct Extracted {
    pub(crate) top_level: BTreeSet<String>,
}

/// zip 里的相对路径（Node `normalizeZipRelativePath`）：去掉末尾的 `/`；不能空、不能有 `\`、`\0`、绝对路径、盘符、空段、`.`、`..`。
pub(crate) fn safe_path(path: &str) -> Result<String, StoreError> {
    let unsafe_path = || StoreError::Invalid(format!("Unsafe plugin zip path: {path}"));
    let trimmed = path.trim_end_matches('/');
    let bytes = trimmed.as_bytes();
    let drive = bytes.len() >= DRIVE_LEN && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if trimmed.is_empty()
        || path.contains('\\')
        || path.contains('\0')
        || path.starts_with('/')
        || drive
    {
        return Err(unsafe_path());
    }
    if trimmed
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(unsafe_path());
    }
    Ok(trimmed.to_owned())
}

/// `relative`（已过 [`safe_path`]）接到 `root` 下。
pub(crate) fn within(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// 读第 `index` 项（阻塞线程里）。
fn read_entry(
    archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>,
    index: usize,
) -> Result<Item, StoreError> {
    let invalid = |message: String| StoreError::Invalid(message);
    let mut file = archive
        .by_index(index)
        .map_err(|error| invalid(format!("Failed to read plugin zip entry: {error}")))?;
    let name = file.name().to_owned();
    let path = safe_path(&name)?;
    if file.encrypted() {
        return Err(invalid(format!(
            "Encrypted plugin zip entries are not supported: {name}"
        )));
    }
    let kind = file.unix_mode().unwrap_or(0) & TYPE_MASK;
    if kind == TYPE_LINK {
        return Err(invalid(format!(
            "Plugin zip entry symlinks are not supported: {name}"
        )));
    }
    if kind != 0 && kind != TYPE_FILE && kind != TYPE_DIR {
        return Err(invalid(format!(
            "Unsupported plugin zip entry type: {name}"
        )));
    }
    if kind == TYPE_DIR || name.ends_with('/') {
        return Ok(Item::Dir(path));
    }
    if file.size() > MAX_FILE_BYTES {
        return Err(invalid(format!(
            "Plugin zip entry exceeds single file limit: {name}"
        )));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(format!("Failed to read plugin zip entry {name}: {error}")))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid(format!(
            "Plugin zip entry exceeds single file limit: {name}"
        )));
    }
    Ok(Item::File(path, bytes))
}

/// 把 `bytes`（一个 zip）解进 `target`（新建）。每项之前看取消。
pub(crate) async fn extract(
    bytes: Vec<u8>,
    target: &Path,
    cancel: &CancellationToken,
) -> Result<Extracted, StoreError> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| {
        StoreError::Invalid(format!("Failed to open plugin zip archive: {error}"))
    })?;
    if archive.len() > MAX_ENTRIES {
        return Err(StoreError::Invalid(format!(
            "Plugin zip has too many entries: {}/{MAX_ENTRIES}",
            archive.len()
        )));
    }
    rust_cli_fs::create_dir_all(target)
        .await
        .map_err(|error| StoreError::io(target, &error))?;
    let mut archive = Some(archive);
    let mut top_level = BTreeSet::new();
    let mut total: u64 = 0;
    for index in 0..archive.as_ref().map_or(0, zip::ZipArchive::len) {
        if cancel.is_cancelled() {
            return Err(StoreError::Cancelled);
        }
        let Some(mut current) = archive.take() else {
            break;
        };
        // 解压是 CPU 活：在阻塞线程里读这一项，再把归档拿回来。
        let (current, item) = rust_cli_base::blocking(move || {
            let item = read_entry(&mut current, index);
            (current, item)
        })
        .await
        .map_err(|error| {
            StoreError::Invalid(format!("Failed to read plugin zip entry: {error}"))
        })?;
        archive = Some(current);
        let item = item?;
        let path = match &item {
            Item::Dir(path) | Item::File(path, _) => path.clone(),
        };
        if let Some(first) = path.split('/').next() {
            top_level.insert(first.to_owned());
        }
        let destination = within(target, &path);
        match item {
            Item::Dir(_) => rust_cli_fs::create_dir_all(&destination).await,
            Item::File(_, bytes) => {
                total += bytes.len() as u64;
                if total > EXTRACT_MAX_BYTES {
                    return Err(StoreError::Invalid(format!(
                        "Plugin zip extracted content exceeds limit: {total}/{EXTRACT_MAX_BYTES}"
                    )));
                }
                rust_cli_fs::write_file(&destination, &bytes).await
            }
        }
        .map_err(|error| StoreError::io(&destination, &error))?;
    }
    Ok(Extracted { top_level })
}

async fn has_manifest(root: &Path) -> bool {
    for path in MANIFEST_PATHS {
        if rust_cli_fs::is_file(&within(root, path)).await {
            return true;
        }
    }
    false
}

/// 解开之后插件的根（Node `resolveZipRoot`）：给了 `path` 用它；根下有 manifest 就是根；`stripRoot` 不是假、只有一个顶层
/// 目录时用它；否则就是解开的目录。`single_root`：GitHub Archive 要求只有一个顶层目录。
pub(crate) async fn zip_root(
    extract_root: &Path,
    extracted: &Extracted,
    path: Option<&str>,
    strip_root: Option<bool>,
    single_root: bool,
) -> Result<PathBuf, StoreError> {
    if single_root && extracted.top_level.len() != 1 {
        return Err(StoreError::Invalid(format!(
            "Plugin zip must contain exactly one top-level directory: {}",
            extracted.top_level.len()
        )));
    }
    if let Some(path) = path {
        let requested = within(extract_root, &safe_path(path)?);
        if !rust_cli_fs::is_dir(&requested).await {
            return Err(StoreError::Invalid(format!(
                "Plugin zip source subdirectory does not exist: {path}"
            )));
        }
        return Ok(requested);
    }
    if has_manifest(extract_root).await {
        return Ok(extract_root.to_path_buf());
    }
    if strip_root != Some(false)
        && let [segment] = extracted.top_level.iter().collect::<Vec<_>>().as_slice()
    {
        let candidate = within(extract_root, segment);
        if rust_cli_fs::is_dir(&candidate).await {
            return Ok(candidate);
        }
    }
    Ok(extract_root.to_path_buf())
}

#[cfg(test)]
#[path = "zip_tests.rs"]
pub(crate) mod tests;
