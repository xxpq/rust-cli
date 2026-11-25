//! `config.json` 插件段的补丁（plugin-store.md §5、§10；PLUGIN-REVIEW K11）。照 Node `file-config.adapter.ts` 的
//! `patchPluginEnabled`、`enablePluginsByDefaultInFileConfig`、`patchPluginRemoved`、`removePluginEnabledFromFileConfig`、
//! `add/removeSuppressedBuiltinInFileConfig`、`patchPluginOptions`：只改 `plugins` 下的这几个键，其余原样。
//!
//! 每个函数收整个文件的顶层对象，返回改没改（没改时调用方不写文件，Node 相同）。持锁读改写由调用方（cli 经 platform 的
//! 写者）做；这里不做 IO。旧的 CUA id 别名（Node `pluginIdAliases`）不处理：Node 读时自己迁。

use serde_json::{Map, Value};

const PLUGINS: &str = "plugins";
const ENABLED: &str = "enabledPlugins";
const OPTIONS: &str = "options";
const SUPPRESSED: &str = "suppressedBuiltins";

/// `parent[key]` 这个对象；没有或不是对象时换成空对象（Node `isRecord(x) ? x : {}`），键的位置不变。
fn object_at<'a>(parent: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let slot = parent
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()));
    if !slot.is_object() {
        *slot = Value::Object(Map::new());
    }
    match slot {
        Value::Object(map) => map,
        _ => unreachable!("the slot was just made an object"),
    }
}

/// `plugins` 段。
fn section(config: &mut Map<String, Value>) -> &mut Map<String, Value> {
    object_at(config, PLUGINS)
}

fn table<'a>(plugins: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    object_at(plugins, key)
}

/// 这一层写了的 `enabledPlugins[id]`（布尔时；没写 = 继承下一层）。
pub fn enabled_of(config: &Map<String, Value>, id: &str) -> Option<bool> {
    config
        .get(PLUGINS)
        .and_then(|plugins| plugins.get(ENABLED))
        .and_then(|enabled| enabled.get(id))
        .and_then(Value::as_bool)
}

/// `enabledPlugins[id] = enabled`（Node 先删再加，键移到末尾）。
pub fn set_enabled(config: &mut Map<String, Value>, id: &str, enabled: bool) -> bool {
    let enabled_plugins = table(section(config), ENABLED);
    enabled_plugins.shift_remove(id);
    enabled_plugins.insert(id.to_owned(), Value::Bool(enabled));
    true
}

/// 删 `enabledPlugins[id]`（恢复继承）。
pub fn clear_enabled(config: &mut Map<String, Value>, id: &str) -> bool {
    let Some(Value::Object(plugins)) = config.get_mut(PLUGINS) else {
        return false;
    };
    match plugins.get_mut(ENABLED) {
        Some(Value::Object(enabled)) => enabled.shift_remove(id).is_some(),
        _ => false,
    }
}

/// 安装后的缺省启用：只给没写过的 id 写 `true`；返回新写的 id。
pub fn default_enable(config: &mut Map<String, Value>, ids: &[String]) -> Vec<String> {
    if ids.is_empty() {
        return Vec::new();
    }
    let already = |config: &Map<String, Value>, id: &str| {
        config
            .get(PLUGINS)
            .and_then(|p| p.get(ENABLED))
            .and_then(Value::as_object)
            .is_some_and(|enabled| enabled.contains_key(id))
    };
    let fresh: Vec<String> = ids
        .iter()
        .filter(|id| !already(config, id))
        .cloned()
        .collect();
    if !fresh.is_empty() {
        let enabled = table(section(config), ENABLED);
        for id in &fresh {
            enabled.insert(id.clone(), Value::Bool(true));
        }
    }
    fresh
}

/// 卸载：删 `enabledPlugins[id]` 与 `options[id]`。
pub fn remove_plugin(config: &mut Map<String, Value>, id: &str) -> bool {
    let Some(Value::Object(plugins)) = config.get_mut(PLUGINS) else {
        return false;
    };
    let mut changed = false;
    for key in [ENABLED, OPTIONS] {
        if let Some(Value::Object(map)) = plugins.get_mut(key) {
            changed |= map.shift_remove(id).is_some();
        }
    }
    changed
}

fn suppressed(config: &Map<String, Value>) -> Vec<String> {
    config
        .get(PLUGINS)
        .and_then(|p| p.get(SUPPRESSED))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// 隐藏内置插件：`suppressedBuiltins` 加这个 id（已在时不改）。
pub fn suppress(config: &mut Map<String, Value>, id: &str) -> bool {
    let mut current = suppressed(config);
    if current.iter().any(|item| item == id) {
        return false;
    }
    current.push(id.to_owned());
    section(config).insert(SUPPRESSED.to_owned(), Value::from(current));
    true
}

/// 恢复内置插件：从 `suppressedBuiltins` 去掉这个 id。
pub fn unsuppress(config: &mut Map<String, Value>, id: &str) -> bool {
    let current = suppressed(config);
    let next: Vec<String> = current.iter().filter(|item| *item != id).cloned().collect();
    if next.len() == current.len() {
        return false;
    }
    section(config).insert(SUPPRESSED.to_owned(), Value::from(next));
    true
}

/// 这一层 `options[id]` 的标量值（敏感值迁移用，Node `readPluginOptionsFromFileConfig`）。
pub fn options_of(config: &Map<String, Value>, id: &str) -> Map<String, Value> {
    config
        .get(PLUGINS)
        .and_then(|p| p.get(OPTIONS))
        .and_then(|o| o.get(id))
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter(|(_, v)| v.is_string() || v.is_number() || v.is_boolean())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// `options[id]`：先删 `clear` 里的键，再按键合并 `set`（Node `patchPluginOptions`）。
pub fn patch_options(
    config: &mut Map<String, Value>,
    id: &str,
    set: &Map<String, Value>,
    clear: &[String],
) -> bool {
    let current = options_of(config, id);
    let mut next: Map<String, Value> = current
        .into_iter()
        .filter(|(key, _)| !clear.contains(key))
        .collect();
    for (key, value) in set {
        next.insert(key.clone(), value.clone());
    }
    let options = table(section(config), OPTIONS);
    options.shift_remove(id);
    options.insert(id.to_owned(), Value::Object(next));
    true
}

#[cfg(test)]
#[path = "config_patch_tests.rs"]
mod tests;
