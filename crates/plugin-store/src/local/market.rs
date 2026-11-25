//! 市场（plugin-store.md §9）：清单缓存的读、锁外取清单进暂存、锁内替换清单缓存与写 `known_marketplaces.json`。
//!
//! 第三方市场的清单缓存是 `marketplaces/<mkt>/`（与 Node 共用，Node 从这里装）：锁内先把旧目录改名成
//! `.<mkt>.rust-old-<pid>-<随机>`，再把暂存改名成目标，最后删旧的；两次改名之间崩溃，下一个持锁的写者把旧的改回（§6.4）。
//! ZCode 官方市场的清单只写 rust 自己的文件（A5：Node 的分片与合并文件归 Node）。

use std::path::{Path, PathBuf};

use rust_cli_base::plugin::sanitize_id;
use tokio_util::sync::CancellationToken;

use super::fetch::{self, Repo};
use super::install::{AUTHORITY_WRITE, read_authority};
use super::stage::{Stage, old_name, old_prefix, sweep_dead};
use super::{LocalStore, Locked, node_tx};
use crate::core::catalog::{self, Catalog};
use crate::core::diagnostic::StoreError;
use crate::core::id::{OFFICIAL_MARKETPLACE, is_official_marketplace};
use crate::core::manifest::{CATALOG_FILE, CATALOG_PATHS};
use crate::core::records::{Known, KnownMarketplaces, NewKnown};
use crate::core::source::{MarketplaceKind, MarketplaceSource, github_clone_url};

/// 暂存容器的名字（清单名在取到之前不知道）。
const STAGE_BASE: &str = "marketplace";

/// 读到的一个市场清单与它的缓存目录（官方市场没有目录）。
#[derive(Debug, Clone)]
pub struct LoadedCatalog {
    pub catalog: Catalog,
    dir: Option<PathBuf>,
}

impl LoadedCatalog {
    /// 相对来源的基准目录（Node `resolveMarketplacePluginBaseDir`）：`metadata.pluginRoot` 在目录里、存在时用它。
    pub(crate) async fn plugin_base(&self) -> Option<PathBuf> {
        let dir = self.dir.clone()?;
        if let Some(root) = &self.catalog.plugin_root {
            let base = rust_cli_base::path::normalize(&dir.join(root.trim_start_matches("./")));
            if base.starts_with(&dir) && rust_cli_fs::is_dir(&base).await {
                return Some(base);
            }
        }
        Some(dir)
    }
}

/// 一个取好、等提交的市场。
#[derive(Debug)]
pub struct StagedMarketplace {
    pub catalog: Catalog,
    source: MarketplaceSource,
    stage: Stage,
}

impl StagedMarketplace {
    pub async fn discard(self) {
        self.stage.discard().await;
    }
}

async fn read_json(path: &Path) -> Result<Option<serde_json::Value>, StoreError> {
    let Some(bytes) = rust_cli_fs::read_optional(path)
        .await
        .map_err(|error| StoreError::io(path, &error))?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| StoreError::Invalid(format!("{}: {error}", path.display())))
}

fn parse_catalog(value: &serde_json::Value) -> Result<Catalog, StoreError> {
    catalog::parse(value)
        .ok_or_else(|| StoreError::Invalid("Marketplace manifest is invalid".to_owned()))
}

/// 目录里的清单文件：`explicit`（仓库来源的 `path`）或缺省的两个位置。
async fn find_catalog(root: &Path, explicit: Option<&str>) -> Option<PathBuf> {
    for candidate in explicit.into_iter().chain(CATALOG_PATHS) {
        let path = super::zip::within(root, candidate.trim_start_matches("./"));
        if path.starts_with(root) && rust_cli_fs::is_file(&path).await {
            return Some(path);
        }
    }
    None
}

impl LocalStore {
    /// 已知的市场（不持锁）。
    pub async fn known_marketplaces(&self) -> Result<Vec<Known>, StoreError> {
        let bytes = read_authority(&self.layout.known()).await?;
        Ok(KnownMarketplaces::parse(bytes.as_deref())?.records())
    }

    /// 一个市场的清单缓存；没有缓存时为 None（不持锁，读者只读）。
    pub async fn catalog(&self, marketplace: &str) -> Result<Option<LoadedCatalog>, StoreError> {
        if marketplace == OFFICIAL_MARKETPLACE {
            let Some(value) = read_json(&self.layout.official_catalog()).await? else {
                return Ok(None);
            };
            return Ok(Some(LoadedCatalog {
                catalog: parse_catalog(&value)?,
                dir: None,
            }));
        }
        let dir = self.layout.marketplace_dir(marketplace);
        let Some(value) = read_json(&dir.join(CATALOG_FILE)).await? else {
            return Ok(None);
        };
        Ok(Some(LoadedCatalog {
            catalog: parse_catalog(&value)?,
            dir: Some(dir),
        }))
    }

    /// 锁外（可取消）：按来源取清单与来源树进暂存。`trusted`：刷新已知市场时它的 id（清单名字要等于它）；新加时为 None
    /// （官方的 id 不能用，Node `trustedId`）。
    pub async fn prepare_marketplace(
        &self,
        source: MarketplaceSource,
        trusted: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<StagedMarketplace, StoreError> {
        let stage = Stage::create(&self.layout.marketplaces(), STAGE_BASE).await?;
        match self
            .fill_marketplace(&stage, &source, trusted, cancel)
            .await
        {
            Ok(catalog) => Ok(StagedMarketplace {
                catalog,
                source,
                stage,
            }),
            Err(error) => {
                stage.discard().await;
                Err(error)
            }
        }
    }

    async fn fill_marketplace(
        &self,
        stage: &Stage,
        source: &MarketplaceSource,
        trusted: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Catalog, StoreError> {
        let content = stage.content();
        let (value, tree, moved) = match &source.kind {
            MarketplaceKind::File { path } => {
                let path = PathBuf::from(path);
                let value = read_json(&path).await?.ok_or_else(|| {
                    StoreError::Invalid(format!(
                        "Marketplace file does not exist: {}",
                        path.display()
                    ))
                })?;
                (value, path.parent().map(Path::to_path_buf), false)
            }
            MarketplaceKind::Directory { path } => {
                let dir = PathBuf::from(path);
                let file = find_catalog(&dir, None).await.ok_or_else(|| {
                    StoreError::Invalid(format!(
                        "Marketplace manifest not found in directory: {path}"
                    ))
                })?;
                (
                    read_json(&file).await?.unwrap_or_default(),
                    Some(dir),
                    false,
                )
            }
            MarketplaceKind::Url { url, headers } => {
                let bytes = fetch::download(
                    &self.egress,
                    url,
                    headers,
                    (fetch::CATALOG_MAX_BYTES, "marketplace manifest"),
                    cancel,
                )
                .await?;
                let value = serde_json::from_slice(&bytes)
                    .map_err(|error| StoreError::Invalid(format!("{url}: {error}")))?;
                (value, None, false)
            }
            MarketplaceKind::Github {
                repo,
                git_ref,
                path,
                sparse,
            }
            | MarketplaceKind::Git {
                url: repo,
                git_ref,
                path,
                sparse,
            } => {
                let url = match &source.kind {
                    MarketplaceKind::Github { .. } => github_clone_url(repo),
                    _ => repo.clone(),
                };
                let fetched = Repo {
                    url: &url,
                    git_ref: git_ref.as_deref(),
                    sha: None,
                    path: None,
                    sparse,
                };
                let root = fetch::repository(&self.egress, &fetched, &stage.work(), cancel).await?;
                let file = find_catalog(&root, path.as_deref()).await.ok_or_else(|| {
                    StoreError::Invalid(format!(
                        "Marketplace manifest not found in repository: {url}"
                    ))
                })?;
                (
                    read_json(&file).await?.unwrap_or_default(),
                    Some(root),
                    true,
                )
            }
            MarketplaceKind::Unsupported(kind) => {
                return Err(StoreError::Unsupported(format!(
                    "Marketplace source is recognized but not supported in this runtime: {kind}"
                )));
            }
        };
        let catalog = parse_catalog(&value)?;
        if is_official_marketplace(&catalog.name) && trusted != Some(catalog.name.as_str()) {
            return Err(StoreError::Invalid(format!(
                "Cannot add a marketplace named \"{}\": that id is reserved for the official marketplace.",
                catalog.name
            )));
        }
        if let Some(trusted) = trusted
            && trusted != catalog.name
        {
            return Err(StoreError::Invalid(format!(
                "Marketplace source must provide {trusted}, received {}",
                catalog.name
            )));
        }
        match tree {
            Some(tree) if moved => rust_cli_fs::rename(&tree, &content)
                .await
                .map_err(|error| StoreError::io(&tree, &error))?,
            Some(tree) if catalog.name != OFFICIAL_MARKETPLACE => {
                rust_cli_fs::copy_tree(&tree, &content, cancel)
                    .await
                    .map_err(|error| StoreError::io(&tree, &error))?;
            }
            _ => rust_cli_fs::create_dir_all(&content)
                .await
                .map_err(|error| StoreError::io(&content, &error))?,
        }
        let file = content.join(CATALOG_FILE);
        rust_cli_fs::write_file(&file, crate::core::json_text(&catalog.raw).as_bytes())
            .await
            .map_err(|error| StoreError::io(&file, &error))?;
        Ok(catalog)
    }
}

impl Locked<'_> {
    /// 持锁读 `known_marketplaces.json`（先收尾它自己的 Node 事务）。
    pub(crate) async fn known(&self) -> Result<KnownMarketplaces, StoreError> {
        let path = self.store.layout.known();
        node_tx::settle(&path).await?;
        let doc = KnownMarketplaces::parse(read_authority(&path).await?.as_deref())?;
        doc.writable()?;
        Ok(doc)
    }

    async fn write_known(&self, doc: &KnownMarketplaces) -> Result<(), StoreError> {
        let path = self.store.layout.known();
        rust_cli_fs::atomic_write(&path, doc.to_text().as_bytes(), AUTHORITY_WRITE)
            .await
            .map_err(|error| StoreError::io(&path, &error))
    }

    /// 补上缺的缺省市场（Node `ensureDefaultPluginMarketplaces`）。
    pub async fn ensure_default_marketplaces(&self, now_ms: u64) -> Result<(), StoreError> {
        let mut doc = self.known().await?;
        if doc.ensure_defaults(&rust_cli_base::js::date::iso_timestamp(now_ms)) {
            self.write_known(&doc).await?;
        }
        Ok(())
    }

    /// 提交一个市场（不可取消）：替换清单缓存，写记录（提交点）。返回写好的记录。
    pub async fn commit_marketplace(
        &self,
        staged: StagedMarketplace,
        now_ms: u64,
    ) -> Result<Known, StoreError> {
        let name = staged.catalog.name.clone();
        let layout = &self.store.layout;
        if name == OFFICIAL_MARKETPLACE {
            let path = layout.official_catalog();
            rust_cli_fs::atomic_write(
                &path,
                crate::core::json_text(&staged.catalog.raw).as_bytes(),
                AUTHORITY_WRITE,
            )
            .await
            .map_err(|error| StoreError::io(&path, &error))?;
        } else {
            let target = layout.marketplace_dir(&name);
            node_tx::settle(&target).await?;
            recover_old(&layout.marketplaces(), &target).await?;
            sweep_dead(&layout.marketplaces()).await;
            let content = staged.stage.content();
            if rust_cli_fs::is_dir(&target).await {
                let old = layout.marketplaces().join(old_name(&sanitize_id(&name)));
                rust_cli_fs::rename(&target, &old)
                    .await
                    .map_err(|error| StoreError::io(&target, &error))?;
                rust_cli_fs::rename(&content, &target)
                    .await
                    .map_err(|error| StoreError::io(&target, &error))?;
                rust_cli_fs::remove_tree(&old)
                    .await
                    .map_err(|error| StoreError::io(&old, &error))?;
            } else {
                rust_cli_fs::rename(&content, &target)
                    .await
                    .map_err(|error| StoreError::io(&target, &error))?;
            }
        }
        staged.stage.discard().await;
        let mut doc = self.known().await?;
        doc.upsert(
            &NewKnown {
                id: name.clone(),
                source: staged.source.raw.clone(),
                description: staged.catalog.description.clone(),
                plugin_count: staged.catalog.plugins.len() as u64,
            },
            &rust_cli_base::js::date::iso_timestamp(now_ms),
        );
        self.write_known(&doc).await?;
        tracing::info!(
            event = "plugins.marketplace.committed",
            marketplace = name,
            "marketplace committed"
        );
        doc.find(&name).ok_or(StoreError::MarketplaceNotFound(name))
    }

    /// 刷新失败：记录上写 `lastRefreshFailure`（Node `persistMarketplaceRefreshFailure`）。取消不写。
    pub async fn record_refresh_failure(
        &self,
        id: &str,
        error: &StoreError,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        if *error == StoreError::Cancelled {
            return Ok(());
        }
        let mut doc = self.known().await?;
        if doc.fail(
            id,
            error.code(),
            &error.to_string(),
            &rust_cli_base::js::date::iso_timestamp(now_ms),
        ) {
            self.write_known(&doc).await?;
        }
        Ok(())
    }

    /// 删市场记录（Node `removeMarketplace`：不删清单缓存与已装的插件）。没有这个市场时为 false。
    pub async fn remove_marketplace(&self, id: &str) -> Result<bool, StoreError> {
        let mut doc = self.known().await?;
        if !doc.remove(id) {
            return Ok(false);
        }
        self.write_known(&doc).await?;
        Ok(true)
    }

    /// 已知的市场（持锁读）。
    pub async fn known_records(&self) -> Result<Vec<Known>, StoreError> {
        Ok(self.known().await?.records())
    }
}

/// 替换中途崩溃留下的旧目录（§6.4）：目标在就删旧的；目标不在就把旧的改回目标。
async fn recover_old(marketplaces: &Path, target: &Path) -> Result<(), StoreError> {
    let Some(name) = target.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Ok(());
    };
    let prefix = old_prefix(&name);
    for entry in rust_cli_fs::subdirectories(marketplaces)
        .await
        .unwrap_or_default()
    {
        if !entry.starts_with(&prefix) {
            continue;
        }
        let old = marketplaces.join(&entry);
        if rust_cli_fs::is_dir(target).await {
            rust_cli_fs::remove_tree(&old)
                .await
                .map_err(|error| StoreError::io(&old, &error))?;
        } else {
            rust_cli_fs::rename(&old, target)
                .await
                .map_err(|error| StoreError::io(&old, &error))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "market_tests.rs"]
pub(crate) mod tests;
