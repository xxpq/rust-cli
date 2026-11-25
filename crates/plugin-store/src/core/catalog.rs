//! 市场清单 `marketplace.json`（plugin-store.md §9.1，Node `parseMarketplaceManifest` / `normalizeMarketplaceManifest`）。
//!
//! `name` 符合名字规则；`plugins` 是数组，或以名字为键的对象；条目要有非空的 `name`。条目原样留在 `raw` 里（合成 manifest、
//! 写记录的 `source` 用）。

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::id::valid_name;

/// 市场清单。
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub name: String,
    pub description: Option<String>,
    pub plugins: Vec<Entry>,
    /// 允许跨市场依赖的市场（`allowCrossMarketplaceDependenciesOn`）。
    pub cross_marketplace: Vec<String>,
    /// `metadata.pluginRoot`：相对来源的基准目录。
    pub plugin_root: Option<String>,
    /// 商店「公开」分段的策展名单（顶层 `featured`，非空字符串，Node 相同）。
    pub featured: Vec<String>,
    /// 规范化之后的原文（`name` 去空白、`plugins` 是数组），写进清单缓存。
    pub raw: Value,
}

/// 清单里的一个插件条目。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub source: Option<Value>,
    pub dependencies: Vec<String>,
    /// 没写时按 `true`（Node：只有显式 `false` 才合成 manifest）。
    pub strict: bool,
    pub raw: Map<String, Value>,
}

fn text(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// 解析清单；不合法时为 None（Node 返回 null，调用方报 `Marketplace manifest is invalid`）。
pub fn parse(value: &Value) -> Option<Catalog> {
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?.trim().to_owned();
    if !valid_name(&name) {
        return None;
    }
    let entries: Vec<Value> = match object.get("plugins") {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(name, plugin)| {
                let mut entry = Map::new();
                entry.insert("name".to_owned(), Value::String(name.clone()));
                if let Some(plugin) = plugin.as_object() {
                    entry.extend(plugin.clone());
                }
                Value::Object(entry)
            })
            .collect(),
        _ => Vec::new(),
    };
    let plugins = entries.iter().filter_map(entry).collect();
    let metadata = object.get("metadata").and_then(Value::as_object);
    let mut raw = object.clone();
    raw.insert("name".to_owned(), Value::String(name.clone()));
    raw.insert("plugins".to_owned(), Value::Array(entries));
    Some(Catalog {
        name,
        description: text(object, "description")
            .or_else(|| metadata.and_then(|m| text(m, "description"))),
        plugins,
        cross_marketplace: object
            .get("allowCrossMarketplaceDependenciesOn")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        plugin_root: metadata.and_then(|m| text(m, "pluginRoot")),
        featured: object
            .get("featured")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|item| !item.trim().is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        raw: Value::Object(raw),
    })
}

fn entry(value: &Value) -> Option<Entry> {
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    Some(Entry {
        name,
        version: text(object, "version"),
        description: text(object, "description"),
        source: object.get("source").cloned(),
        dependencies: object
            .get("dependencies")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(dependency).collect())
            .unwrap_or_default(),
        strict: object
            .get("strict")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        raw: object.clone(),
    })
}

/// 依赖的写法（Node `normalizeDependencyRef`）：字符串（去掉末尾的 `@^版本`），或 `{name, marketplace?}`。
fn dependency(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(match text.rfind("@^") {
            Some(at) if !text[at + 2..].contains('@') => text[..at].to_owned(),
            _ => text.clone(),
        }),
        Value::Object(object) => {
            let name = text(object, "name")?.trim().to_owned();
            if name.is_empty() {
                return None;
            }
            let marketplace = text(object, "marketplace").unwrap_or_default();
            let marketplace = marketplace.trim();
            Some(if marketplace.is_empty() {
                name
            } else {
                format!("{name}@{marketplace}")
            })
        }
        _ => None,
    }
}

impl Catalog {
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.plugins.iter().find(|entry| entry.name == name)
    }
}

/// 条目在商店里的展示字段（Node `parseEntryStoreListing`）：解析不到有效内容时没有。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub display_name: Option<String>,
    pub display_name_i18n: BTreeMap<String, String>,
    pub description_i18n: BTreeMap<String, String>,
    pub icon: Option<String>,
    pub category: Option<String>,
    pub homepage: Option<String>,
    pub privacy_policy: Option<String>,
    pub terms_of_service: Option<String>,
    pub hero_image: Option<String>,
    pub author: Option<String>,
    pub author_url: Option<String>,
    pub example_prompts: Vec<String>,
    pub example_prompts_i18n: BTreeMap<String, Vec<String>>,
    /// 只认显式的 `true`（Node：歧义的写法一律按不要付费套餐）。
    pub requires_paid_plan: bool,
}

/// 目录自报、只有装之前才看得到的组件种类（Node `CATALOG_CLAIMED_COMPONENT_TYPES`）。
const CLAIMED_COMPONENT_TYPES: [&str; 2] = ["ui", "extension"];
/// 条目里的组件键与它对应的种类（Node `inferComponentTypes` 的顺序）。
const COMPONENT_KEYS: [(&str, &str); 6] = [
    ("agents", "agent"),
    ("commands", "command"),
    ("skills", "skill"),
    ("hooks", "hook"),
    ("mcpServers", "mcp"),
    ("lspServers", "lsp"),
];

/// 非空白的字符串。
fn filled(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn string_map(object: &Map<String, Value>, key: &str) -> BTreeMap<String, String> {
    object
        .get(key)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(locale, text)| Some((locale.clone(), text.as_str()?.to_owned())))
        .collect()
}

/// 作者：字符串，或 `{name, url}`（Node `normalizeAuthorValue`）。返回名字与主页。
pub fn author(value: Option<&Value>) -> (Option<String>, Option<String>) {
    let trimmed = |text: &str| Some(text.trim().to_owned()).filter(|text| !text.is_empty());
    match value {
        Some(Value::String(name)) => (trimmed(name), None),
        Some(Value::Object(object)) => (
            object.get("name").and_then(Value::as_str).and_then(trimmed),
            object.get("url").and_then(Value::as_str).and_then(trimmed),
        ),
        _ => (None, None),
    }
}

impl Entry {
    /// 商店的展示字段（Node `parseEntryStoreListing`）。
    pub fn listing(&self) -> Option<Listing> {
        let raw = &self.raw;
        let (author, author_url) = author(raw.get("author"));
        let listing = Listing {
            display_name: filled(raw, "displayName"),
            display_name_i18n: string_map(raw, "displayName_i18n"),
            description_i18n: string_map(raw, "description_i18n"),
            icon: filled(raw, "icon"),
            category: filled(raw, "category"),
            homepage: filled(raw, "homepage"),
            privacy_policy: filled(raw, "privacyPolicy"),
            terms_of_service: filled(raw, "termsOfService"),
            hero_image: filled(raw, "heroImage"),
            author,
            author_url,
            example_prompts: raw
                .get("examplePrompts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned)
                .collect(),
            example_prompts_i18n: raw
                .get("examplePrompts_i18n")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(locale, list)| {
                    let items: Vec<String> = list
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    (!items.is_empty()).then(|| (locale.clone(), items))
                })
                .collect(),
            requires_paid_plan: raw.get("requiresPaidPlan") == Some(&Value::Bool(true)),
        };
        (listing != Listing::default()).then_some(listing)
    }

    /// 条目声明的组件种类（Node `inferComponentTypes`）：组件键在条目里就算，另加目录自报的 `ui`、`extension`。
    pub fn component_types(&self) -> Vec<String> {
        let claimed = self.raw.get("componentTypes").and_then(Value::as_array);
        COMPONENT_KEYS
            .iter()
            .filter(|(key, _)| self.raw.contains_key(*key))
            .map(|(_, kind)| (*kind).to_owned())
            .chain(
                CLAIMED_COMPONENT_TYPES
                    .iter()
                    .filter(|kind| {
                        claimed.is_some_and(|items| items.iter().any(|item| item == **kind))
                    })
                    .map(|kind| (*kind).to_owned()),
            )
            .collect()
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
