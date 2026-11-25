//! 插件存储的读侧（ext.md §4.12；用户决定 2026-11-12 Q5：拉取接口，本机实现读 Node 的文件，云上的实现由那边提供）：
//! 内置插件根、已安装的记录与敏感值。以前分在 platform 的 `plugin_inventory`（文件格式）与 cli 的 `PluginStore`（接口），
//! 一种文件格式两个 crate 都懂（PLUGIN-REVIEW F17），现在都在这里。
//!
//! 只读：不做 Node 的原子目录崩溃恢复（ext.md D24），损坏的私有文件也不备份（读者只读，F11）。插件目录里的内容由 ext 经
//! 执行环境端口读。

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use rust_cli_base::path::normalize;
use rust_cli_base::plugin::sanitize_id;
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value};

use super::install::read_authority;
use crate::core::id::OFFICIAL_MARKETPLACE;
use crate::core::records::{Installed, Record};
use crate::core::secrets::{PluginSecrets, SecretsFile};

/// 存储根下的插件缓存目录与官方市场的分片（Node 播种的，过渡期读，ext.md §4.1 来源 3′）。
const CACHE_DIR: &str = "cache";
const MARKETPLACES_DIR: &str = "marketplaces";
const BUNDLED_PARTITION: &str = "bundled-marketplace.json";
/// 分片的格式版本。
const PARTITION_VERSION: u64 = 1;

/// 读侧给的值（ext.md §4.12）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginIntents {
    /// 内置插件根：App 交来的内置目录的子目录，没有时是 Node 播种的官方缓存（ext.md §4.1 来源 3、3′）。
    pub official: Vec<PathBuf>,
    /// 已安装插件：marketplace 与目录。
    pub installed: Vec<(String, PathBuf)>,
    pub secrets: PluginSecrets,
}

/// 插件存储的读侧接口。
#[async_trait]
pub trait PluginStore: Send + Sync {
    async fn intents(&self) -> PluginIntents;
}

/// 本机实现：插件存储根下的 Node 文件、cli 存储根下的私有文件与 App 交来的内置目录。
pub struct LocalPluginStore {
    pub root: StoragePath,
    pub secrets: StoragePath,
    /// `ZCODE_BUILTIN_PLUGINS_DIR`（ext.md §4.13）。
    pub builtin: Option<PathBuf>,
}

#[async_trait]
impl PluginStore for LocalPluginStore {
    async fn intents(&self) -> PluginIntents {
        let official = match &self.builtin {
            // 给了内置目录就不读 Node 的缓存，读不出也不回落（ext.md §4.13）。
            Some(dir) => builtin_plugin_roots(dir).await.unwrap_or_else(|error| {
                tracing::warn!(dir = %dir.display(), %error, "the built-in plugin directory could not be read");
                Vec::new()
            }),
            None => official_plugin_roots(&self.root).await,
        };
        let installed = installed_plugins(&self.root)
            .await
            .iter()
            .map(|record| {
                (
                    record.marketplace.clone(),
                    installed_root(&self.root, record),
                )
            })
            .collect();
        let secrets = match plugin_secrets(&self.secrets).await {
            SecretsRead::Read(secrets) => secrets,
            SecretsRead::Missing => PluginSecrets::default(),
            SecretsRead::Unreadable(reason) => {
                tracing::warn!(
                    path = %self.secrets.as_path().display(),
                    reason,
                    "the plugin secrets file is unreadable; sensitive plugin options are ignored"
                );
                PluginSecrets::default()
            }
        };
        PluginIntents {
            official,
            installed,
            secrets,
        }
    }
}

/// `installed_plugins.json` 的合格记录，按文件顺序；文件不存在或不合法时为空（读者不报错，ext.md §4.2）。
pub async fn installed_plugins(root: &StoragePath) -> Vec<Record> {
    let path = root.as_path().join(crate::core::records::INSTALLED_FILE);
    match read_authority(&path).await {
        Ok(bytes) => Installed::parse(bytes.as_deref())
            .map(|doc| doc.records())
            .unwrap_or_default(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "plugin inventory file could not be read");
            Vec::new()
        }
    }
}

/// 已安装插件的目录（Node `resolveInstalledPluginRoot`）：`installPath`，为空时 `<存储根>/cache/<mkt>/<name>/<version>`。
pub fn installed_root(root: &StoragePath, record: &Record) -> PathBuf {
    if !record.install_path.is_empty() {
        return PathBuf::from(&record.install_path);
    }
    root.as_path()
        .join(CACHE_DIR)
        .join(sanitize_id(&record.marketplace))
        .join(sanitize_id(&record.name))
        .join(sanitize_id(&record.version))
}

fn text<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

/// bundled 分片里的插件目录（Node `loadBundledOfficialPluginRootsSync`）：`cachePath` 必须在
/// `<存储根>/cache/zcode-plugins-official/<name>/` 之下。分片版本不是 1 或 `manifest` 不是对象时为 None。
fn bundled_roots(root: &StoragePath, partition: &Map<String, Value>) -> Option<Vec<PathBuf>> {
    if partition.get("version").and_then(Value::as_u64) != Some(PARTITION_VERSION) {
        return None;
    }
    let manifest = partition.get("manifest")?.as_object()?;
    let cache = root.as_path().join(CACHE_DIR).join(OFFICIAL_MARKETPLACE);
    let plugins = manifest.get("plugins").and_then(Value::as_array);
    let inside = |parent: &Path, child: &Path| {
        child
            .strip_prefix(parent)
            .is_ok_and(|rest| rest.components().next().is_some())
    };
    let roots = plugins
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|plugin| {
            let name = text(plugin, "name").filter(|name| !name.is_empty())?;
            let cache_path = text(plugin, "cachePath")?;
            let plugin_root = normalize(&cache.join(name));
            let resolved = normalize(Path::new(cache_path));
            (inside(&cache, &plugin_root) && inside(&plugin_root, &resolved)).then_some(resolved)
        })
        .collect();
    Some(roots)
}

/// 官方内置插件的目录：bundled 分片的 `cachePath`；分片不存在时扫描 `cache/zcode-plugins-official/<插件>/<版本>`。
pub async fn official_plugin_roots(root: &StoragePath) -> Vec<PathBuf> {
    let partition = root
        .as_path()
        .join(MARKETPLACES_DIR)
        .join(OFFICIAL_MARKETPLACE)
        .join(BUNDLED_PARTITION);
    if let Ok(Some(bytes)) = rust_cli_fs::read_optional(&partition).await
        && let Ok(Value::Object(partition)) = serde_json::from_slice::<Value>(&bytes)
        && let Some(roots) = bundled_roots(root, &partition)
    {
        return roots;
    }
    let cache = root.as_path().join(CACHE_DIR).join(OFFICIAL_MARKETPLACE);
    let mut roots = Vec::new();
    for plugin in subdirectories(&cache).await {
        let dir = cache.join(plugin);
        roots.extend(subdirectories(&dir).await.into_iter().map(|v| dir.join(v)));
    }
    roots
}

/// 子目录名；目录不存在时为空，其余错误记警告后为空。
async fn subdirectories(dir: &Path) -> Vec<String> {
    match rust_cli_fs::subdirectories(dir).await {
        Ok(names) => names,
        Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            tracing::warn!(dir = %dir.display(), %error, "the official plugin cache could not be scanned");
            Vec::new()
        }
    }
}

/// App 交来的内置插件目录里的插件根：每个子目录一个，按名字排序。读不出时是错误（调用方记 warn，不回落）。
pub async fn builtin_plugin_roots(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut names = rust_cli_fs::subdirectories(dir).await?;
    names.sort();
    Ok(names.into_iter().map(|name| dir.join(name)).collect())
}

/// 读 `plugin-secrets.json` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretsRead {
    /// 文件不存在：没有敏感值。
    Missing,
    /// 读不出或不符合 Node 的 schema：整份当空（调用方记 warn），原因不带值。
    Unreadable(String),
    Read(PluginSecrets),
}

/// 读 `plugin-secrets.json`（只读，不加锁：写是原子替换，读者只会看到旧文件或新文件）。
pub async fn plugin_secrets(path: &StoragePath) -> SecretsRead {
    match rust_cli_fs::read_optional(path.as_path()).await {
        Ok(None) => SecretsRead::Missing,
        Ok(Some(bytes)) => match SecretsFile::parse(&bytes) {
            Some(file) => SecretsRead::Read(file.redacted()),
            None => SecretsRead::Unreadable(
                "the file does not match the plugin secrets schema".to_owned(),
            ),
        },
        Err(error) => SecretsRead::Unreadable(error.kind().to_string()),
    }
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;
