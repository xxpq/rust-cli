//! 安装、更新、卸载的存储一侧（plugin-store.md §5）：锁外 [`LocalStore::prepare_install`] 取依赖闭包里每个插件的内容进暂存；
//! 锁内 [`Locked::commit_install`] 改名进不可变目录、原子替换 `installed_plugins.json`（提交点）、给旧目录打孤儿标记。

use std::path::{Path, PathBuf};

use rust_cli_base::path::normalize;
use rust_cli_base::plugin::sanitize_id;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::fetch::{self, Repo};
use super::market::LoadedCatalog;
use super::stage::{Stage, digest_tree, sweep_dead};
use super::zip::{extract, zip_root};
use super::{LocalStore, Locked, node_tx, orphans};
use crate::core::closure::{Member, closure};
use crate::core::diagnostic::StoreError;
use crate::core::digest::{self, PACKAGE_MARKER};
use crate::core::id::PluginId;
use crate::core::manifest::{self, MANIFEST_PATHS, SYNTHESIZED_MANIFEST};
use crate::core::records::{Installed, NewRecord, Record};
use crate::core::source::{PluginSource, plugin_source};

/// 权威文件的写法：原子替换，父目录按需建，Node 的改名重试。
pub(crate) const AUTHORITY_WRITE: rust_cli_fs::WriteOptions = rust_cli_fs::WriteOptions {
    mode: rust_cli_fs::FileMode::Default,
    temp_name: rust_cli_fs::TempName::Hidden,
    parent: rust_cli_fs::ParentDir::Create,
    rename_retry: rust_cli_fs::NODE_RENAME_RETRY,
};
/// Node 在 Windows 上替换失败时留下的备份名的后缀（K5）。
const NODE_BACKUP: &str = ".backup";
/// zip 来源的 sha256 是 64 位 hex（Node `ZIP_REQUIRED_SHA256_PATTERN`）。
const SHA256_HEX_LEN: usize = 64;

/// 读一个权威文件：主文件不在而 Node 的备份 `.<名>.backup` 在时读备份（K5）。
pub(crate) async fn read_authority(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    if let Some(bytes) = rust_cli_fs::read_optional(path)
        .await
        .map_err(|error| StoreError::io(path, &error))?
    {
        return Ok(Some(bytes));
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(None);
    };
    let backup = parent.join(format!(".{}{NODE_BACKUP}", name.to_string_lossy()));
    rust_cli_fs::read_optional(&backup)
        .await
        .map_err(|error| StoreError::io(&backup, &error))
}

/// 一个取好、等提交的插件。
#[derive(Debug)]
pub struct StagedPlugin {
    pub id: PluginId,
    pub version: String,
    pub digest: String,
    dependencies: Option<Vec<String>>,
    source: Option<Value>,
    stage: Stage,
}

/// 依赖闭包里的插件都取好了，等提交。
#[derive(Debug)]
pub struct Prepared {
    /// 闭包（依赖在前，最后是要装的那个）。
    pub closure: Vec<PluginId>,
    pub staged: Vec<StagedPlugin>,
}

impl Prepared {
    /// 不提交了（取消、失败）：删掉暂存。
    pub async fn discard(self) {
        for staged in self.staged {
            staged.stage.discard().await;
        }
    }
}

/// 一个提交好的插件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub id: PluginId,
    pub version: String,
    pub install_path: PathBuf,
    /// 同 id 的旧记录（更新时）。
    pub previous: Option<Record>,
}

/// `base` 下的相对路径（Node `resolveInside`）：越出时为 None。
fn inside(base: &Path, relative: &str) -> Option<PathBuf> {
    let joined = normalize(&base.join(relative.trim_start_matches("./")));
    joined.starts_with(normalize(base)).then_some(joined)
}

async fn read_manifest(root: &Path) -> Result<Option<(String, Value)>, StoreError> {
    for path in MANIFEST_PATHS {
        let file = super::zip::within(root, path);
        if let Some(bytes) = rust_cli_fs::read_optional(&file)
            .await
            .map_err(|error| StoreError::io(&file, &error))?
        {
            let value = serde_json::from_slice(&bytes).map_err(|error| {
                StoreError::ManifestInvalid(format!("{}: {error}", file.display()))
            })?;
            return Ok(Some((file.to_string_lossy().into_owned(), value)));
        }
    }
    Ok(None)
}

impl LocalStore {
    /// 锁外（可取消）：`root` 的依赖闭包，每个插件取进暂存目录、合成 manifest、算摘要、写摘要标记。
    pub async fn prepare_install(
        &self,
        root: &PluginId,
        cancel: &CancellationToken,
    ) -> Result<Prepared, StoreError> {
        let first = self
            .catalog(&root.marketplace)
            .await?
            .ok_or_else(|| StoreError::MarketplaceNotFound(root.marketplace.clone()))?;
        let mut catalogs = vec![first];
        for marketplace in catalogs[0].catalog.cross_marketplace.clone() {
            if let Some(loaded) = self.catalog(&marketplace).await? {
                catalogs.push(loaded);
            }
        }
        let find = |name: &str| catalogs.iter().find(|loaded| loaded.catalog.name == name);
        let members = closure(root, |name| find(name).map(|loaded| &loaded.catalog))?;
        let mut staged = Vec::new();
        for member in &members {
            let Some(loaded) = find(&member.id.marketplace) else {
                continue;
            };
            match self.stage_member(member, loaded, cancel).await {
                Ok(plugin) => staged.push(plugin),
                Err(error) => {
                    Prepared {
                        closure: Vec::new(),
                        staged,
                    }
                    .discard()
                    .await;
                    return Err(error);
                }
            }
        }
        Ok(Prepared {
            closure: members.into_iter().map(|member| member.id).collect(),
            staged,
        })
    }

    /// 一个市场条目在本机已有的目录（插件管理的详情，plugin-store.md §12.1）：相对市场缓存的、没写来源的（同名目录）、
    /// `directory` 来源的。远端的（git、zip）、市场没有缓存或没有这个条目时为 None。只读，不联网。
    pub async fn local_entry_dir(&self, plugin: &PluginId) -> Result<Option<PathBuf>, StoreError> {
        let Some(loaded) = self.catalog(&plugin.marketplace).await? else {
            return Ok(None);
        };
        let Some(entry) = loaded.catalog.find(&plugin.name) else {
            return Ok(None);
        };
        let label = plugin.to_string();
        let base = loaded.plugin_base().await;
        let dir = match plugin_source(entry.source.as_ref(), &label)? {
            PluginSource::Relative(relative) => {
                base.as_deref().and_then(|base| inside(base, &relative))
            }
            PluginSource::ByName => base.map(|base| base.join(&entry.name)),
            PluginSource::Directory(path) => Some(PathBuf::from(path)),
            _ => None,
        };
        Ok(match dir {
            Some(dir) if rust_cli_fs::is_dir(&dir).await => Some(dir),
            _ => None,
        })
    }

    async fn stage_member(
        &self,
        member: &Member,
        loaded: &LoadedCatalog,
        cancel: &CancellationToken,
    ) -> Result<StagedPlugin, StoreError> {
        let parent = self
            .layout
            .plugin_dir(&member.id.marketplace, &member.id.name);
        let stage = Stage::create(&parent, &sanitize_id(&member.id.name)).await?;
        match self.fill(&stage, member, loaded, cancel).await {
            Ok((version, digest)) => Ok(StagedPlugin {
                id: member.id.clone(),
                version,
                digest,
                dependencies: (!member.entry.dependencies.is_empty())
                    .then(|| member.entry.dependencies.clone()),
                source: member.entry.source.clone(),
                stage,
            }),
            Err(error) => {
                stage.discard().await;
                Err(error)
            }
        }
    }

    /// 取内容进 `stage.content()`，返回（版本，摘要）。
    async fn fill(
        &self,
        stage: &Stage,
        member: &Member,
        loaded: &LoadedCatalog,
        cancel: &CancellationToken,
    ) -> Result<(String, String), StoreError> {
        let entry = &member.entry;
        let label = member.id.to_string();
        let content = stage.content();
        let move_into = |from: PathBuf| {
            let content = content.clone();
            async move {
                rust_cli_fs::rename(&from, &content)
                    .await
                    .map_err(|error| StoreError::io(&from, &error))
            }
        };
        let source = plugin_source(entry.source.as_ref(), &label)?;
        let zip = matches!(source, PluginSource::Zip { .. });
        match source {
            PluginSource::Relative(relative) => {
                let base = loaded.plugin_base().await;
                let dir = match base.as_deref().and_then(|base| inside(base, &relative)) {
                    Some(dir) => dir,
                    None if Path::new(&relative).is_absolute() => PathBuf::from(&relative),
                    None => {
                        return Err(StoreError::Invalid(format!(
                            "Unsupported or missing plugin source: {relative}"
                        )));
                    }
                };
                self.copy_dir(&dir, &content, &relative, cancel).await?;
            }
            PluginSource::ByName => {
                let Some(base) = loaded.plugin_base().await else {
                    return Err(StoreError::Invalid(format!(
                        "Plugin source is not supported for {label}"
                    )));
                };
                self.copy_dir(&base.join(&entry.name), &content, &label, cancel)
                    .await?;
            }
            PluginSource::Directory(path) => {
                self.copy_dir(Path::new(&path), &content, &path, cancel)
                    .await?
            }
            PluginSource::Repo {
                url,
                git_ref,
                path,
                sha,
            } => {
                let repo = Repo {
                    url: &url,
                    git_ref: git_ref.as_deref(),
                    sha: sha.as_deref(),
                    path: path.as_deref(),
                    sparse: &[],
                };
                let root = fetch::repository(&self.egress, &repo, &stage.work(), cancel).await?;
                let dir = match &path {
                    Some(sub) => inside(&root, sub).ok_or_else(|| {
                        StoreError::Invalid(format!(
                            "Plugin source subdirectory does not exist: {sub}"
                        ))
                    })?,
                    None => root,
                };
                if !rust_cli_fs::is_dir(&dir).await {
                    return Err(StoreError::Invalid(format!(
                        "Plugin source subdirectory does not exist: {}",
                        path.unwrap_or_default()
                    )));
                }
                move_into(dir).await?;
            }
            PluginSource::Zip {
                url,
                sha256,
                path,
                strip_root,
                headers,
            } => {
                let expected = sha256.to_ascii_lowercase();
                if expected.len() != SHA256_HEX_LEN
                    || !expected.chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(StoreError::Invalid(
                        "Plugin zip source sha256 must be a 64 character hex string".to_owned(),
                    ));
                }
                let bytes = fetch::download(
                    &self.egress,
                    &url,
                    &headers,
                    (fetch::ZIP_MAX_BYTES, "plugin zip"),
                    cancel,
                )
                .await?;
                let actual = rust_cli_base::hash::sha256_hex(&bytes);
                if actual != expected {
                    return Err(StoreError::Fetch(format!(
                        "Plugin zip sha256 mismatch: expected={expected}, actual={actual}"
                    )));
                }
                let target = stage.work().join("zip");
                let extracted = extract(bytes, &target, cancel).await?;
                let root =
                    zip_root(&target, &extracted, path.as_deref(), strip_root, false).await?;
                move_into(root).await?;
            }
        }
        let mut manifest = read_manifest(&content).await?;
        if manifest.is_none() && !entry.strict {
            let synthesized = manifest::synthesize(entry);
            let file = super::zip::within(&content, SYNTHESIZED_MANIFEST);
            rust_cli_fs::write_file(&file, crate::core::json_text(&synthesized).as_bytes())
                .await
                .map_err(|error| StoreError::io(&file, &error))?;
            manifest = Some((file.to_string_lossy().into_owned(), synthesized));
        }
        // zip 的来源要求 manifest 的名字等于条目的名字（Node `assertZipPluginInstallRoot`）。
        if zip {
            let Some((_, value)) = &manifest else {
                return Err(StoreError::ManifestMissing(format!(
                    "Plugin manifest not found: {label}"
                )));
            };
            let name = manifest::name(value).map_err(StoreError::ManifestInvalid)?;
            if name != entry.name {
                return Err(StoreError::ManifestInvalid(format!(
                    "Plugin manifest name '{name}' does not match marketplace entry '{}'",
                    entry.name
                )));
            }
        }
        let version = manifest::installed_version(manifest.as_ref().map(|(_, value)| value), entry);
        let digest = digest_tree(&content).await?;
        let marker = content.join(PACKAGE_MARKER);
        rust_cli_fs::write_file(
            &marker,
            digest::marker(&label, &digest, entry.source.as_ref()).as_bytes(),
        )
        .await
        .map_err(|error| StoreError::io(&marker, &error))?;
        Ok((version, digest))
    }

    async fn copy_dir(
        &self,
        from: &Path,
        to: &Path,
        shown: &str,
        cancel: &CancellationToken,
    ) -> Result<(), StoreError> {
        if !rust_cli_fs::is_dir(from).await {
            return Err(StoreError::Invalid(format!(
                "Plugin source directory does not exist: {shown}"
            )));
        }
        rust_cli_fs::copy_tree(from, to, cancel)
            .await
            .map_err(|error| StoreError::io(from, &error))
    }

    /// 已安装的记录（不持锁，读者只读）。
    pub async fn installed_records(&self) -> Result<Vec<Record>, StoreError> {
        let bytes = read_authority(&self.layout.installed()).await?;
        Ok(Installed::parse(bytes.as_deref())?.records())
    }
}

impl Locked<'_> {
    /// 持锁读 `installed_plugins.json`（先收尾它自己的 Node 事务）。
    pub(crate) async fn installed(&self) -> Result<Installed, StoreError> {
        let path = self.store.layout.installed();
        node_tx::settle(&path).await?;
        let doc = Installed::parse(read_authority(&path).await?.as_deref())?;
        doc.writable()?;
        Ok(doc)
    }

    /// 持锁读到的记录（提交之后读回）。
    pub async fn records(&self) -> Result<Vec<Record>, StoreError> {
        Ok(self.installed().await?.records())
    }

    async fn write_installed(&self, doc: &Installed) -> Result<(), StoreError> {
        let path = self.store.layout.installed();
        rust_cli_fs::atomic_write(&path, doc.to_text().as_bytes(), AUTHORITY_WRITE)
            .await
            .map_err(|error| StoreError::io(&path, &error))
    }

    /// 一个暂存的插件改名进 `cache/<mkt>/<name>/<ver>-<摘要>/`（已存在 = 同内容，去掉孤儿标记），记录写进内存里的文档。
    async fn place(
        &self,
        doc: &mut Installed,
        staged: StagedPlugin,
        now: &str,
    ) -> Result<Committed, StoreError> {
        let parent = self
            .store
            .layout
            .plugin_dir(&staged.id.marketplace, &staged.id.name);
        sweep_dead(&parent).await;
        let previous = doc.find(&staged.id.to_string());
        if let Some(previous) = &previous {
            node_tx::settle(Path::new(&previous.install_path)).await?;
        }
        let target = parent.join(digest::dir_name(&staged.version, &staged.digest));
        if rust_cli_fs::is_dir(&target).await {
            orphans::unmark(&target).await?;
        } else {
            let content = staged.stage.content();
            rust_cli_fs::rename(&content, &target)
                .await
                .map_err(|error| StoreError::io(&target, &error))?;
        }
        staged.stage.discard().await;
        doc.upsert(
            &NewRecord {
                id: staged.id.clone(),
                version: staged.version.clone(),
                install_path: target.to_string_lossy().into_owned(),
                dependencies: staged.dependencies,
                source: staged.source,
            },
            now,
        );
        Ok(Committed {
            id: staged.id,
            version: staged.version,
            install_path: target,
            previous,
        })
    }

    /// 提交（不可取消）：改名进 `cache/<mkt>/<name>/<ver>-<摘要>/`（已存在 = 同内容），写记录（提交点），旧目录打孤儿标记。
    pub async fn commit_install(
        &self,
        prepared: Prepared,
        now_ms: u64,
    ) -> Result<Vec<Committed>, StoreError> {
        let now = rust_cli_base::js::date::iso_timestamp(now_ms);
        let mut doc = match self.installed().await {
            Ok(doc) => doc,
            Err(error) => {
                prepared.discard().await;
                return Err(error);
            }
        };
        let mut committed = Vec::new();
        let mut pending = prepared.staged.into_iter();
        while let Some(staged) = pending.next() {
            match self.place(&mut doc, staged, &now).await {
                Ok(item) => committed.push(item),
                Err(error) => {
                    // 没提交的暂存都删掉（已改名进最终目录的没人引用，有摘要标记，清理时补孤儿标记）。
                    Prepared {
                        closure: Vec::new(),
                        staged: pending.collect(),
                    }
                    .discard()
                    .await;
                    return Err(error);
                }
            }
        }
        self.write_installed(&doc).await?;
        for item in &committed {
            if let Some(previous) = &item.previous {
                self.orphan(&previous.install_path, &doc, now_ms).await?;
            }
        }
        tracing::info!(
            event = "plugins.install.committed",
            plugins = committed.len(),
            "plugin install committed"
        );
        Ok(committed)
    }

    /// 卸载的记录一侧：删记录（提交点），旧目录打孤儿标记。没有这条记录时为 None。
    pub async fn remove_install(
        &self,
        id: &str,
        now_ms: u64,
    ) -> Result<Option<Record>, StoreError> {
        let mut doc = self.installed().await?;
        let Some(record) = doc.find(id) else {
            return Ok(None);
        };
        node_tx::settle(Path::new(&record.install_path)).await?;
        doc.remove(id);
        self.write_installed(&doc).await?;
        self.orphan(&record.install_path, &doc, now_ms).await?;
        tracing::info!(
            event = "plugins.uninstall.committed",
            id,
            "plugin record removed"
        );
        Ok(Some(record))
    }

    /// 删数据目录（卸载，没有 `keep_data` 时）。
    pub async fn remove_data(&self, id: &str) -> Result<(), StoreError> {
        let dir = self.store.layout.data_dir(id);
        rust_cli_fs::remove_tree(&dir)
            .await
            .map_err(|error| StoreError::io(&dir, &error))
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
