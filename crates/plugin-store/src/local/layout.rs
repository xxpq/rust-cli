//! 存储根下的路径（plugin-store.md §3.2，PLUGIN-REVIEW K1）。各段按 Node `sanitizePluginId` 净化。

use std::path::{Path, PathBuf};

use rust_cli_base::plugin::sanitize_id;
use rust_cli_paths::StoragePath;

use crate::core::records::{INSTALLED_FILE, KNOWN_FILE};

/// 存储根下的目录名。
const CACHE_DIR: &str = "cache";
const MARKETPLACES_DIR: &str = "marketplaces";
const DATA_DIR: &str = "data";
/// rust 自己的官方市场清单缓存（只 rust 读；Node 的官方市场文件归 Node，A5）。
const OFFICIAL_CATALOG: &str = "rust-official-marketplace.json";

/// 一个插件存储根的路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
    secrets: PathBuf,
}

impl Layout {
    /// `root` 是插件存储根（`<storage.dir>/cli/plugins`），`secrets` 是 `plugin-secrets.json`（paths 给）。
    pub fn new(root: &StoragePath, secrets: &StoragePath) -> Self {
        Self {
            root: root.as_path().to_path_buf(),
            secrets: secrets.as_path().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn installed(&self) -> PathBuf {
        self.root.join(INSTALLED_FILE)
    }

    pub fn known(&self) -> PathBuf {
        self.root.join(KNOWN_FILE)
    }

    pub fn cache(&self) -> PathBuf {
        self.root.join(CACHE_DIR)
    }

    pub fn marketplaces(&self) -> PathBuf {
        self.root.join(MARKETPLACES_DIR)
    }

    /// 第三方市场的清单缓存目录（Node `getMarketplaceManifestPath` 的目录）。
    pub fn marketplace_dir(&self, marketplace: &str) -> PathBuf {
        self.marketplaces().join(sanitize_id(marketplace))
    }

    pub fn official_catalog(&self) -> PathBuf {
        self.root.join(OFFICIAL_CATALOG)
    }

    /// 一个插件的版本目录的父目录：`cache/<mkt>/<name>`。
    pub fn plugin_dir(&self, marketplace: &str, name: &str) -> PathBuf {
        self.cache()
            .join(sanitize_id(marketplace))
            .join(sanitize_id(name))
    }

    /// 数据目录（Node `getPluginDataDir`）。
    pub fn data_dir(&self, plugin_id: &str) -> PathBuf {
        self.root.join(DATA_DIR).join(sanitize_id(plugin_id))
    }

    pub fn secrets(&self) -> &Path {
        &self.secrets
    }
}
