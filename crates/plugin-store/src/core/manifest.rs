//! 插件 manifest 与市场清单的位置、名字与版本、按条目合成 manifest（plugin-store.md §9.3，PLUGIN-REVIEW K14）。

use serde_json::{Map, Value};

use super::catalog::Entry;
use super::id::valid_name;

/// 插件 manifest 的位置，按顺序找（Node `findPluginManifestPath`）。
pub const MANIFEST_PATHS: [&str; 3] = [
    ".zcode-plugin/plugin.json",
    ".claude-plugin/plugin.json",
    ".codex-plugin/plugin.json",
];
/// 合成 manifest 写的位置（Node `ensureMarketplaceEntryManifest`）。
pub const SYNTHESIZED_MANIFEST: &str = ".claude-plugin/plugin.json";
/// 市场清单的位置，按顺序找（Node `findMarketplaceManifestPath`）。
pub const CATALOG_PATHS: [&str; 2] = [".claude-plugin/marketplace.json", "marketplace.json"];
/// 清单缓存里规范清单的文件名。
pub const CATALOG_FILE: &str = "marketplace.json";
/// manifest 里只诊断、不运行的组件（Node `UNSUPPORTED_MANIFEST_FIELDS`）。
pub const UNSUPPORTED_FIELDS: [&str; 4] = ["channels", "lspServers", "outputStyles", "settings"];

/// 合成 manifest 时从条目里去掉的键：市场的分类与商店的展示字段（Node `createManifestFromMarketplaceEntry`）。
const CATALOG_ONLY_KEYS: [&str; 15] = [
    "source",
    "category",
    "tags",
    "strict",
    "displayName",
    "displayName_i18n",
    "description_i18n",
    "icon",
    "privacyPolicy",
    "termsOfService",
    "heroImage",
    "examplePrompts",
    "examplePrompts_i18n",
    "requiresPaidPlan",
    "extensionSummary",
];

/// manifest 的名字（Node `readPluginManifestFromRoot`）：去首尾空白后合规；文字照 Node。
pub fn name(manifest: &Value) -> Result<String, String> {
    let Some(object) = manifest.as_object() else {
        return Err("Plugin manifest must be a JSON object".to_owned());
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if valid_name(&name) {
        Ok(name)
    } else {
        Err(format!("Invalid plugin name: {name}"))
    }
}

/// 安装记录与目录名用的版本（Node `resolveInstalledPluginVersion`）：manifest 的非空 `version` → 条目的 → `0.0.0`。
pub fn installed_version(manifest: Option<&Value>, entry: &Entry) -> String {
    manifest
        .and_then(|m| m.get("version"))
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| entry.version.clone())
        .unwrap_or_else(|| super::DEFAULT_VERSION.to_owned())
}

/// 按条目合成的 manifest（`strict: false` 且目录里没有 manifest 时）。
pub fn synthesize(entry: &Entry) -> Value {
    let mut raw: Map<String, Value> = entry.raw.clone();
    for key in CATALOG_ONLY_KEYS {
        raw.shift_remove(key);
    }
    raw.insert("name".to_owned(), Value::String(entry.name.clone()));
    raw.insert(
        "version".to_owned(),
        Value::String(
            entry
                .version
                .clone()
                .unwrap_or_else(|| super::DEFAULT_VERSION.to_owned()),
        ),
    );
    Value::Object(raw)
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
