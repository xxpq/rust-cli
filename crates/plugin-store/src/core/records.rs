//! 两个权威文件的文档（plugin-store.md §3、§5、§9.2；PLUGIN-REVIEW K2、K3、K5、K13、M21）：
//! `installed_plugins.json` 与 `known_marketplaces.json`。
//!
//! 读：两种格式都认（Node 的数组与以 id 为键的对象，Node 也认后者），版本不管。写：Node 格式（`{"version": 1, …: [...]}`），保留别人
//! 的记录、记录里的未知字段与顶层的未知字段。`version` 写了又不是 1 的文件不写（[`Installed::writable`]：可能是以后的新格式，
//! 写了就坏了，M21）；不是 JSON 对象的文件也不写（Node 会当空的覆盖掉，这里留给用户处理）。

use serde_json::{Map, Value, json};

use super::diagnostic::StoreError;
use super::id::{COMPAT_OFFICIAL_MARKETPLACE, OFFICIAL_MARKETPLACE, PluginId};
use super::source::MarketplaceSource;

/// 文件格式的版本（Node 写 1）。
pub const RECORDS_VERSION: u64 = 1;
/// 记录里 Node 的事务 id：改写这条记录时去掉（指向的旧目录已收尾，plugin-store.md §7）。
pub const TRANSACTION_ID: &str = "cacheTransactionId";

/// 文件的字节（不存在时为 None）→ 顶层对象；不是 JSON 对象时是错误（读者当空，写者不写）。
fn top(bytes: Option<&[u8]>, file: &str) -> Result<Map<String, Value>, StoreError> {
    let Some(bytes) = bytes else {
        return Ok(Map::new());
    };
    let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(bytes) else {
        return Err(StoreError::Invalid(format!(
            "{file} is not a JSON object; fix or remove it before changing plugins"
        )));
    };
    Ok(object)
}

/// 写之前的版本保护（M21）：`version` 没写或是 1 才写。
fn writable(rest: &Map<String, Value>, file: &str) -> Result<(), StoreError> {
    match rest.get("version") {
        None => Ok(()),
        Some(version) if version.as_u64() == Some(RECORDS_VERSION) => Ok(()),
        Some(version) => Err(StoreError::Invalid(format!(
            "{file} has an unsupported version {version}; it is left unchanged"
        ))),
    }
}

fn text(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// 写出的文字：`{"version": 1, <键>: [...], …其余顶层键}`。
fn document(mut rest: Map<String, Value>, key: &str, items: Vec<Value>) -> String {
    let mut object = Map::new();
    object.insert("version".to_owned(), json!(RECORDS_VERSION));
    object.insert(key.to_owned(), Value::Array(items));
    rest.shift_remove("version");
    rest.shift_remove(key);
    object.extend(rest);
    super::json_text(&Value::Object(object))
}

// ── installed_plugins.json ──

/// 记录文件名（存储根下）。
pub const INSTALLED_FILE: &str = "installed_plugins.json";

/// 一条安装记录（Node `InstalledPluginRecord` 的必填字段，K2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub marketplace: String,
    pub version: String,
    pub install_path: String,
    pub installed_at: String,
    pub scope: String,
}

impl Record {
    /// Node `isInstalledPluginRecord`：七个字段的类型都对。
    fn of(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let scope = text(object, "scope")?;
        if scope != "user" && scope != "workspace" {
            return None;
        }
        Some(Self {
            id: text(object, "id")?,
            name: text(object, "name")?,
            marketplace: text(object, "marketplace")?,
            version: text(object, "version")?,
            install_path: text(object, "installPath")?,
            installed_at: text(object, "installedAt")?,
            scope,
        })
    }
}

/// 一次安装要写的记录。
#[derive(Debug, Clone, PartialEq)]
pub struct NewRecord {
    pub id: PluginId,
    pub version: String,
    pub install_path: String,
    /// 条目的 `dependencies`（写了时）与 `source`（原样）。
    pub dependencies: Option<Vec<String>>,
    pub source: Option<Value>,
}

/// `installed_plugins.json` 的文档。
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    rest: Map<String, Value>,
    plugins: Vec<Value>,
}

impl Installed {
    pub fn parse(bytes: Option<&[u8]>) -> Result<Self, StoreError> {
        let mut rest = top(bytes, INSTALLED_FILE)?;
        let plugins = match rest.shift_remove("plugins") {
            Some(Value::Array(items)) => items,
            Some(Value::Object(map)) => map
                .iter()
                .flat_map(|(id, entry)| from_map(id, entry))
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self { rest, plugins })
    }

    /// 合格的记录，按文件顺序。
    pub fn records(&self) -> Vec<Record> {
        self.plugins.iter().filter_map(Record::of).collect()
    }

    pub fn find(&self, id: &str) -> Option<Record> {
        self.records().into_iter().find(|record| record.id == id)
    }

    /// 写进一条记录（同 id 的旧记录：保留它的未知字段与 `installedAt`，去掉 `cacheTransactionId`）。
    pub fn upsert(&mut self, record: &NewRecord, now: &str) {
        let id = record.id.to_string();
        let index = self
            .plugins
            .iter()
            .position(|value| value.get("id").and_then(Value::as_str) == Some(id.as_str()));
        let mut object = index
            .and_then(|index| self.plugins[index].as_object().cloned())
            .unwrap_or_default();
        object.shift_remove(TRANSACTION_ID);
        let installed_at = text(&object, "installedAt").unwrap_or_else(|| now.to_owned());
        object.insert("id".into(), id.into());
        object.insert("name".into(), record.id.name.clone().into());
        object.insert("marketplace".into(), record.id.marketplace.clone().into());
        object.insert("version".into(), record.version.clone().into());
        object.insert("installPath".into(), record.install_path.clone().into());
        object.insert("installedAt".into(), installed_at.into());
        object.insert("updatedAt".into(), now.into());
        object.insert("scope".into(), "user".into());
        if let Some(dependencies) = &record.dependencies {
            object.insert("dependencies".into(), json!(dependencies));
        }
        if let Some(source) = &record.source {
            object.insert("source".into(), source.clone());
        }
        match index {
            Some(index) => self.plugins[index] = Value::Object(object),
            None => self.plugins.push(Value::Object(object)),
        }
    }

    /// 删掉这个 id 的记录（合格的才算），返回它。
    pub fn remove(&mut self, id: &str) -> Option<Record> {
        let index = self
            .plugins
            .iter()
            .position(|value| Record::of(value).is_some_and(|r| r.id == id))?;
        Record::of(&self.plugins.remove(index))
    }

    /// 还引用这个目录的记录有没有。
    pub fn references(&self, path: &str) -> bool {
        self.records()
            .iter()
            .any(|record| record.install_path == path)
    }

    /// 这条记录上 Node 留下的事务 id。
    pub fn transaction_of(&self, id: &str) -> Option<String> {
        self.plugins
            .iter()
            .find(|value| value.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|value| value.get(TRANSACTION_ID))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// 能不能写回（版本保护）。
    pub fn writable(&self) -> Result<(), StoreError> {
        writable(&self.rest, INSTALLED_FILE)
    }

    pub fn to_text(&self) -> String {
        document(self.rest.clone(), "plugins", self.plugins.clone())
    }
}

/// 以 id 为键的对象格式的一项（Node `normalizeInstalledPluginRecordFromMap`）：转成 Node 的记录。
fn from_map(id: &str, entry: &Value) -> Vec<Value> {
    let Some(parsed) = PluginId::parse(id) else {
        return Vec::new();
    };
    let entries: Vec<&Value> = match entry {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    entries
        .into_iter()
        .filter_map(Value::as_object)
        .filter_map(|item| {
            let install_path = text(item, "installPath").filter(|p| !p.is_empty())?;
            let scope = match item.get("scope").and_then(Value::as_str) {
                Some("project" | "local") => "workspace",
                _ => "user",
            };
            let mut record = json!({
                "id": id,
                "name": parsed.name,
                "marketplace": parsed.marketplace,
                "version": text(item, "version").unwrap_or_else(|| super::DEFAULT_VERSION.to_owned()),
                "installPath": install_path,
                "installedAt": text(item, "installedAt").unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_owned()),
                "scope": scope,
            });
            if let Some(updated) = text(item, "lastUpdated") {
                record["updatedAt"] = updated.into();
            }
            Some(record)
        })
        .collect()
}

// ── known_marketplaces.json ──

/// 市场记录文件名（存储根下）。
pub const KNOWN_FILE: &str = "known_marketplaces.json";
/// ZCode 官方市场的清单地址（`packages/shared/src/plugin-marketplaces.ts`）。
pub const OFFICIAL_CATALOG_URL: &str = rust_cli_base::hosts::ZCODE_OFFICIAL_PLUGIN_CATALOG;
/// 兼容插件格式的官方市场的 GitHub 仓库。
pub const COMPAT_OFFICIAL_REPO: &str = "anthropics/claude-plugins-official";

/// 缺省市场（Node `DEFAULT_PLUGIN_MARKETPLACES`）：id、来源的写法、说明。
pub const DEFAULT_MARKETPLACES: [(&str, &str, &str); 2] = [
    (
        OFFICIAL_MARKETPLACE,
        OFFICIAL_CATALOG_URL,
        "Official ZCode plugins marketplace: built-in and community plugins for ZCode.",
    ),
    (
        COMPAT_OFFICIAL_MARKETPLACE,
        COMPAT_OFFICIAL_REPO,
        "Directory of popular Claude Code extensions including development tools, productivity plugins, and MCP integrations",
    ),
];

/// 一条市场记录（Node `KnownMarketplaceRecord`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Known {
    pub id: String,
    pub name: String,
    pub source: MarketplaceSource,
    pub description: Option<String>,
    pub added_at: Option<String>,
    pub last_updated: Option<String>,
    pub plugin_count: u64,
    /// `lastRefreshFailure` 原样。
    pub refresh_failure: Option<Value>,
}

impl Known {
    /// Node `isKnownMarketplaceRecord`。
    fn of(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let source = object.get("source").filter(|s| s.is_object())?;
        Some(Self {
            id: text(object, "id")?,
            name: text(object, "name")?,
            source: MarketplaceSource::from_json(source),
            description: text(object, "description"),
            added_at: text(object, "addedAt"),
            last_updated: text(object, "lastUpdated"),
            plugin_count: object.get("pluginCount").and_then(Value::as_f64)? as u64,
            refresh_failure: object.get("lastRefreshFailure").cloned(),
        })
    }
}

/// 刷新成功后要写的市场记录。
#[derive(Debug, Clone, PartialEq)]
pub struct NewKnown {
    pub id: String,
    pub source: Value,
    pub description: Option<String>,
    pub plugin_count: u64,
}

/// `known_marketplaces.json` 的文档。
#[derive(Debug, Clone, PartialEq)]
pub struct KnownMarketplaces {
    rest: Map<String, Value>,
    marketplaces: Vec<Value>,
}

impl KnownMarketplaces {
    pub fn parse(bytes: Option<&[u8]>) -> Result<Self, StoreError> {
        let mut rest = top(bytes, KNOWN_FILE)?;
        let marketplaces = match rest.shift_remove("marketplaces") {
            Some(Value::Array(items)) => items,
            Some(Value::Object(map)) => map.into_iter().map(|(_, value)| value).collect(),
            _ => Vec::new(),
        };
        Ok(Self { rest, marketplaces })
    }

    pub fn records(&self) -> Vec<Known> {
        self.marketplaces.iter().filter_map(Known::of).collect()
    }

    pub fn find(&self, id: &str) -> Option<Known> {
        self.records().into_iter().find(|record| record.id == id)
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.marketplaces
            .iter()
            .position(|value| value.get("id").and_then(Value::as_str) == Some(id))
    }

    /// 刷新成功（Node `upsertKnownMarketplace`）：保留旧记录的 `addedAt` 与未知字段，去掉失败与事务 id。
    pub fn upsert(&mut self, record: &NewKnown, now: &str) {
        let index = self.index(&record.id);
        let mut object = index
            .and_then(|index| self.marketplaces[index].as_object().cloned())
            .unwrap_or_default();
        object.shift_remove(TRANSACTION_ID);
        object.shift_remove("lastRefreshFailure");
        let added_at = text(&object, "addedAt").unwrap_or_else(|| now.to_owned());
        object.insert("id".into(), record.id.clone().into());
        object.insert("source".into(), record.source.clone());
        object.insert("name".into(), record.id.clone().into());
        match &record.description {
            Some(description) => object.insert("description".into(), description.clone().into()),
            None => object.shift_remove("description"),
        };
        object.insert("addedAt".into(), added_at.into());
        object.insert("lastUpdated".into(), now.into());
        object.insert("pluginCount".into(), json!(record.plugin_count));
        match index {
            Some(index) => self.marketplaces[index] = Value::Object(object),
            None => self.marketplaces.push(Value::Object(object)),
        }
    }

    /// 刷新失败（Node `persistMarketplaceRefreshFailure`）：只改这一个字段；没有这个记录时什么都不做。
    pub fn fail(&mut self, id: &str, code: &str, message: &str, now: &str) -> bool {
        let Some(object) = self
            .index(id)
            .and_then(|index| self.marketplaces[index].as_object_mut())
        else {
            return false;
        };
        object.insert(
            "lastRefreshFailure".into(),
            json!({"code": code, "failedAt": now, "message": message}),
        );
        true
    }

    pub fn remove(&mut self, id: &str) -> bool {
        match self.index(id) {
            Some(index) => {
                self.marketplaces.remove(index);
                true
            }
            None => false,
        }
    }

    /// 补上缺的缺省市场（Node `ensureDefaultPluginMarketplaces`）；补了返回 true。
    pub fn ensure_defaults(&mut self, now: &str) -> bool {
        let mut changed = false;
        for (id, source, description) in DEFAULT_MARKETPLACES {
            if self.index(id).is_some() {
                continue;
            }
            self.marketplaces.push(json!({
                "id": id,
                "source": MarketplaceSource::default_from(source).raw,
                "name": id,
                "description": description,
                "addedAt": now,
                "pluginCount": 0,
            }));
            changed = true;
        }
        changed
    }

    /// 这个市场记录上 Node 留下的事务 id。
    pub fn transaction_of(&self, id: &str) -> Option<String> {
        self.index(id)
            .and_then(|index| self.marketplaces[index].get(TRANSACTION_ID))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// 能不能写回（版本保护）。
    pub fn writable(&self) -> Result<(), StoreError> {
        writable(&self.rest, KNOWN_FILE)
    }

    pub fn to_text(&self) -> String {
        document(self.rest.clone(), "marketplaces", self.marketplaces.clone())
    }
}

/// 权威文件里有没有这个事务 id（Node `containsTransaction`：任何一层对象的 `cacheTransactionId`）。
pub fn contains_transaction(value: &Value, transaction: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.get(TRANSACTION_ID).and_then(Value::as_str) == Some(transaction)
                || object
                    .values()
                    .any(|value| contains_transaction(value, transaction))
        }
        Value::Array(items) => items
            .iter()
            .any(|value| contains_transaction(value, transaction)),
        _ => false,
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
