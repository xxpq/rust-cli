//! `plugins` 段、插件存储的读侧与插件来源（docs/specs/rust-cli/config.md §2.1、ext.md §4.0、§4.1、§4.9、§4.12）。
//!
//! 合并按 Node `config-merger.ts`：`enabled` 按键覆盖，`dirs` 拼接去重，`enabledPlugins` 按 id 覆盖，
//! `suppressedBuiltins` 由最后写它的层整体替换；`options` 按层保留（敏感值要按层叠加私有文件）；`extraKnownMarketplaces` 只读用户级
//! （不进加载，只给插件管理的列表与显式刷新 / 安装，plugin-store.md §9.5）。
//! 项目层到这里之前已经只剩 `enabledPlugins` 与 `options`（读文件时的投影，config.md §1.1）。
//!
//! 两步分开（ext.md §4.12）：[`PluginStore::intents`] 读插件存储（本机实现读 Node 的文件，在 plugin-store），[`sources`] 是
//! 纯函数，用意图、`plugins` 段与执行侧的情况算出候选、启停、配置值与数据根。

use std::collections::BTreeMap;

use rust_cli_base::plugin::{PluginOption, PluginOptions};
use rust_cli_ext::mcp::config_dir;
use rust_cli_ext::plugins::{PluginCandidate, PluginOrigin, PluginSources};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::permission::RuleSource;
use rust_cli_paths::StoragePath;
// 插件存储的读侧接口与本机实现、工作区键在 plugin-store（plugin-store.md §2：一种文件格式只有一个 crate 懂）。
pub(crate) use rust_cli_plugin_store::core::secrets::project_key;
use rust_cli_plugin_store::core::secrets::{PluginSecrets, SecretValues};
use rust_cli_plugin_store::core::source::MarketplaceSource;
pub(crate) use rust_cli_plugin_store::local::inventory::{
    LocalPluginStore, PluginIntents, PluginStore,
};
use serde_json::Value;

use super::merge::Layer;
use super::official_plugins;

/// Node `ZCODE_INLINE_PLUGIN_MARKETPLACE` 与 `ZCODE_OFFICIAL_PLUGIN_MARKETPLACE`。
const INLINE: &str = rust_cli_base::names::INLINE_MARKETPLACE;
const OFFICIAL: &str = rust_cli_base::names::OFFICIAL_MARKETPLACE;

/// 一层配置值对应私有文件的哪一部分（ext.md §4.9）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SecretsScope {
    /// 用户级配置：私有文件的 `user`。
    User,
    /// 项目层：私有文件的 `workspaces[<键>]`（[`project_key`] 算的）。
    Workspace(String),
    /// `--settings`：没有敏感值。
    None,
}

/// 一层的 `plugins.options`（插件 id → 键 → 值的文字）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptionLayer {
    pub(crate) secrets: SecretsScope,
    pub(crate) options: BTreeMap<String, BTreeMap<String, String>>,
}

/// 合并后的 `plugins` 段。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PluginSection {
    pub(crate) enabled: bool,
    pub(crate) dirs: Vec<String>,
    pub(crate) enabled_plugins: BTreeMap<String, bool>,
    pub(crate) suppressed_builtins: Vec<String>,
    /// 各层的配置值，按优先级从低到高（用户级、项目各层、`--settings`）。
    pub(crate) option_layers: Vec<OptionLayer>,
    /// 用户级声明的市场（id → 来源，相对路径已按用户配置所在的目录解析，plugin-store.md §9.5）。
    pub(crate) declared_marketplaces: BTreeMap<String, MarketplaceSource>,
}

/// 插件配置的快照（config.md §3）：段与意图两份值，加插件存储根（数据根的位置）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginConfig {
    pub(crate) section: PluginSection,
    pub(crate) intents: PluginIntents,
    pub(crate) root: StoragePath,
}

/// 执行侧的情况（第二段按执行侧布局给，ext.md §4.0）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionSide {
    /// 执行侧的数据根；None 时插件没有数据目录。
    pub(crate) data_root: Option<EnvPath>,
    /// 执行侧与控制侧是同一个文件系统：插件存储里的内置与已安装插件在执行侧可读（主 env 在本进程里）。
    pub(crate) shares_storage: bool,
}

/// 非空字符串数组里的合格项；坏项记警告（D1）。
fn strings(
    path: &str,
    key: &str,
    value: &Value,
    warnings: &mut Vec<String>,
) -> Option<Vec<String>> {
    let Some(items) = value.as_array() else {
        warnings.push(format!("{path}: plugins.{key} must be an array of strings"));
        return None;
    };
    let mut found = Vec::new();
    for item in items {
        match item.as_str().filter(|text| !text.is_empty()) {
            Some(text) => found.push(text.to_owned()),
            None => warnings.push(format!("{path}: plugins.{key} skips {item}")),
        }
    }
    Some(found)
}

/// 一层的 `plugins.options`：值只认字符串、数、布尔，别的记警告跳过（D1）。
fn options(
    path: &str,
    value: &Value,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut found = BTreeMap::new();
    let Some(plugins) = value.as_object() else {
        warnings.push(format!("{path}: plugins.options must be an object"));
        return found;
    };
    for (id, keys) in plugins {
        let Some(keys) = keys.as_object() else {
            warnings.push(format!("{path}: plugins.options.{id} must be an object"));
            continue;
        };
        let mut plugin = BTreeMap::new();
        for (key, value) in keys {
            match PluginOption::scalar(value) {
                Some(text) => {
                    plugin.insert(key.clone(), text);
                }
                None => warnings.push(format!(
                    "{path}: plugins.options.{id}.{key} must be a string, number or boolean"
                )),
            }
        }
        found.insert(id.clone(), plugin);
    }
    found
}

/// 一层对应私有文件的哪一部分：用户级是 `user`，项目层按它的基准目录（`.zcode/config.json` 是 `.zcode` 的上一层），
/// `--settings` 没有。
fn secrets_scope(layer: &Layer<'_>) -> SecretsScope {
    match layer.source {
        RuleSource::UserSettings => SecretsScope::User,
        RuleSource::ProjectSettings => {
            SecretsScope::Workspace(project_key(&config_dir(layer.path)))
        }
        _ => SecretsScope::None,
    }
}

/// 合并各层的 `plugins` 段。
pub(crate) fn merge(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> PluginSection {
    let mut merged = PluginSection {
        enabled: true,
        dirs: Vec::new(),
        enabled_plugins: BTreeMap::new(),
        suppressed_builtins: Vec::new(),
        option_layers: Vec::new(),
        declared_marketplaces: BTreeMap::new(),
    };
    for layer in layers {
        // 每一层都对应私有文件的一部分，没写 `options` 的层也是（Node 的 `optionLayers` 每个配置文件一层）。
        let mut option_layer = OptionLayer {
            secrets: secrets_scope(layer),
            options: BTreeMap::new(),
        };
        let section = layer.object.get("plugins");
        if let Some(value) = section.and_then(|section| section.get("options")) {
            option_layer.options = options(layer.path, value, warnings);
        }
        merged.option_layers.push(option_layer);
        let Some(section) = section else {
            continue;
        };
        let Some(section) = section.as_object() else {
            warnings.push(format!("{}: plugins must be an object", layer.path));
            continue;
        };
        if let Some(value) = section.get("enabled") {
            match value.as_bool() {
                Some(enabled) => merged.enabled = enabled,
                None => warnings.push(format!("{}: plugins.enabled must be a boolean", layer.path)),
            }
        }
        if let Some(dirs) = section
            .get("dirs")
            .and_then(|value| strings(layer.path, "dirs", value, warnings))
        {
            for dir in dirs {
                if !merged.dirs.contains(&dir) {
                    merged.dirs.push(dir);
                }
            }
        }
        match section.get("enabledPlugins").map(Value::as_object) {
            Some(Some(table)) => {
                for (id, on) in table {
                    match on.as_bool() {
                        Some(on) => {
                            merged.enabled_plugins.insert(id.clone(), on);
                        }
                        None => warnings.push(format!(
                            "{}: plugins.enabledPlugins.{id} must be a boolean",
                            layer.path
                        )),
                    }
                }
            }
            Some(None) => warnings.push(format!(
                "{}: plugins.enabledPlugins must be an object",
                layer.path
            )),
            None => {}
        }
        if let Some(suppressed) = section
            .get("suppressedBuiltins")
            .and_then(|value| strings(layer.path, "suppressedBuiltins", value, warnings))
        {
            merged.suppressed_builtins = suppressed;
        }
        // 只有用户级生效（Node 的合并删掉项目层的这个键）；`--settings` 也不读
        if layer.source == RuleSource::UserSettings
            && let Some(value) = section.get(EXTRA_MARKETPLACES)
        {
            declared(
                layer.path,
                value,
                &mut merged.declared_marketplaces,
                warnings,
            );
        }
    }
    merged
}

/// 用户级声明市场的键。
const EXTRA_MARKETPLACES: &str = "extraKnownMarketplaces";
/// 来源里按用户配置所在目录解析相对路径的两种（Node `resolveDeclaredMarketplaceSource`）。
const LOCAL_SOURCES: [&str; 2] = ["file", "directory"];

/// 一层的 `extraKnownMarketplaces`：`{<id>: {source: {...}}}`；`file` / `directory` 的相对 `path` 相对这个配置文件所在的目录。坏项记警告（D1）。
fn declared(
    path: &str,
    value: &Value,
    found: &mut BTreeMap<String, MarketplaceSource>,
    warnings: &mut Vec<String>,
) {
    let Some(table) = value.as_object() else {
        warnings.push(format!(
            "{path}: plugins.{EXTRA_MARKETPLACES} must be an object"
        ));
        return;
    };
    let style = PathStyle::infer(path);
    let base = style.dirname(path);
    for (id, declaration) in table {
        let Some(mut source) = declaration
            .get("source")
            .and_then(Value::as_object)
            .cloned()
        else {
            warnings.push(format!(
                "{path}: plugins.{EXTRA_MARKETPLACES}.{id} must have a source object"
            ));
            continue;
        };
        let local = source
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|kind| LOCAL_SOURCES.contains(&kind));
        if local && let Some(relative) = source.get("path").and_then(Value::as_str) {
            let resolved = style.resolve(&base, relative, None);
            source.insert("path".to_owned(), Value::String(resolved));
        }
        found.insert(
            id.clone(),
            MarketplaceSource::from_json(&Value::Object(source)),
        );
    }
}

/// 一层的敏感值：私有文件里这一层对应的部分（工作区按 [`project_key`] 比较，同一个键的多份按文件顺序合并）。
fn layer_secrets<'a>(secrets: &'a PluginSecrets, scope: &SecretsScope) -> Vec<&'a SecretValues> {
    match scope {
        SecretsScope::User => vec![&secrets.user],
        SecretsScope::Workspace(key) => secrets
            .workspaces
            .iter()
            .filter(|(root, _)| project_key(root) == *key)
            .map(|(_, values)| values)
            .collect(),
        SecretsScope::None => Vec::new(),
    }
}

/// 存下的配置值（ext.md §4.9）：一个键从高层往低层找，第一个有值的层生效；同一层里私有文件的值优先于明文（Node
/// `applyPluginSecrets`）。私有文件里的值是秘密类型。
fn resolve_options(
    layers: &[OptionLayer],
    secrets: &PluginSecrets,
) -> BTreeMap<String, PluginOptions> {
    let mut resolved: BTreeMap<String, PluginOptions> = BTreeMap::new();
    for layer in layers {
        for (id, keys) in &layer.options {
            let plugin = resolved.entry(id.clone()).or_default();
            for (key, value) in keys {
                plugin.insert(key.clone(), PluginOption::Plain(value.clone()));
            }
        }
        for values in layer_secrets(secrets, &layer.secrets) {
            for (id, keys) in values {
                let plugin = resolved.entry(id.clone()).or_default();
                for (key, value) in keys {
                    plugin.insert(key.clone(), PluginOption::Secret(value.clone()));
                }
            }
        }
    }
    resolved
}

/// 配置目录的位置（Node `resolve(rootPath)`）：绝对路径原样，相对路径相对工作区根；`.` 与 `..` 按字面
/// 化简，不展开 `~`。目录在执行侧（deployment.md §21），按工作区根的路径风格拼接，不用 controller 的 `std::path`。
pub(crate) fn resolve_dir(root: &str, dir: &str) -> String {
    PathStyle::infer(root).resolve(root, dir, None)
}

/// 第二段的插件来源（ext.md §4.0、§4.1）：候选按 Node `resolveCandidates` 的顺序，启停、官方表、配置值与数据根。内置与
/// 已安装插件在控制侧的存储里，执行侧与它不是同一个文件系统时不产生这两种候选（deployment.md §24）。
pub(crate) fn sources(config: &PluginConfig, root: &str, side: ExecutionSide) -> PluginSources {
    let section = &config.section;
    let mut candidates = Vec::new();
    if section.enabled {
        let candidate = |dir: String, marketplace: &str, origin| PluginCandidate {
            dir: EnvPath::new(dir),
            marketplace: marketplace.to_owned(),
            origin,
        };
        for dir in &section.dirs {
            candidates.push(candidate(
                resolve_dir(root, dir),
                INLINE,
                PluginOrigin::ConfigDir,
            ));
        }
        if side.shares_storage {
            // 同一个文件系统：控制侧的存储路径就是执行侧的路径。官方市场的已安装插件排在内置插件之前：同 id 先到先得，
            // 用户显式装的那个生效（A15，plugin-store.md §8.2）；别的已安装插件仍排在内置之后（Node 的顺序）。
            let installed = |official: bool| {
                config
                    .intents
                    .installed
                    .iter()
                    .filter(move |(marketplace, _)| (marketplace == OFFICIAL) == official)
            };
            for (marketplace, dir) in installed(true) {
                let dir = dir.to_string_lossy().into_owned();
                candidates.push(candidate(dir, marketplace, PluginOrigin::Installed));
            }
            for dir in &config.intents.official {
                let dir = dir.to_string_lossy().into_owned();
                candidates.push(candidate(dir, OFFICIAL, PluginOrigin::Official));
            }
            for (marketplace, dir) in installed(false) {
                let dir = dir.to_string_lossy().into_owned();
                candidates.push(candidate(dir, marketplace, PluginOrigin::Installed));
            }
        }
    }
    PluginSources {
        candidates,
        enabled_plugins: section.enabled_plugins.clone(),
        suppressed_builtins: section.suppressed_builtins.clone(),
        official: official_plugins::table(),
        options: resolve_options(&section.option_layers, &config.intents.secrets),
        data_root: side.data_root,
    }
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
