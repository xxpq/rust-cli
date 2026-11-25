//! `plugin-secrets.json`（plugin-store.md §10；PLUGIN-REVIEW K7；Node `plugin-secrets.adapter.ts`）：
//! `{version: 1, user: {id: {k: v}}, workspaces: {<键>: {id: {k: v}}}}`，值是标量，0600。schema 是 strict：别的顶层键、
//! 版本不是 1、值不是标量，整份不合法。
//!
//! 读侧要秘密类型（[`SecretsFile::redacted`]），写侧要原值，所以文件的模型存 JSON 值；写只在 `local::secrets_file` 的
//! 持锁读改写里。

use std::collections::BTreeMap;

use rust_cli_base::plugin::PluginOption;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::env_files::PathStyle;
use serde_json::{Map, Value, json};

/// 文件名（cli 存储根下）。
pub const SECRETS_FILE: &str = "plugin-secrets.json";
/// Node `PLUGIN_SECRETS_FILE_VERSION`。
const SECRETS_VERSION: u64 = 1;
/// 顶层的三个键（strict）。
const SECRETS_KEYS: [&str; 3] = ["version", "user", "workspaces"];

/// 工作区维度的键（ext.md §4.12，A16）：一层项目配置的基准目录换成 `workspaces` 的键。本机照 Node
/// `normalizePluginConfigPathForComparison`：Windows 路径规范化、`\` 换成 `/`、转小写；其余规范化。私有文件里已有的键按同一个
/// 函数比较。云上的键由控制面的项目身份给（PLUGIN-CLOUD PP5），换这一个函数。
pub fn project_key(path: &str) -> String {
    let style = PathStyle::infer(path);
    let normalized = style.normalize(path);
    match style {
        PathStyle::Windows => normalized.replace('\\', "/").to_lowercase(),
        PathStyle::Posix => normalized,
    }
}

/// 插件 id → 键 → 敏感值（读侧）。
pub type SecretValues = BTreeMap<String, BTreeMap<String, RedactedString>>;

/// 读侧的视图：用户级与按文件里的工作区键（原样）分的敏感值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSecrets {
    pub user: SecretValues,
    /// 文件里的工作区键（原样）与它的值，按文件里的顺序；比较用 [`project_key`]。
    pub workspaces: Vec<(String, SecretValues)>,
}

/// 写到哪一层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    User,
    /// 工作区：[`project_key`] 算好的键。
    Workspace(String),
}

/// 文件的模型（值是 JSON 标量）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SecretsFile {
    user: Map<String, Value>,
    workspaces: Map<String, Value>,
}

/// 一层的值合不合 schema（`{id: {k: 标量}}`）。
fn valid_layer(value: &Value) -> bool {
    value.as_object().is_some_and(|plugins| {
        plugins.values().all(|keys| {
            keys.as_object()
                .is_some_and(|keys| keys.values().all(|v| PluginOption::scalar(v).is_some()))
        })
    })
}

fn redact(layer: &Map<String, Value>) -> SecretValues {
    layer
        .iter()
        .map(|(id, keys)| {
            let values = keys
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(key, value)| {
                    Some((
                        key.clone(),
                        RedactedString::new(PluginOption::scalar(value)?),
                    ))
                })
                .collect();
            (id.clone(), values)
        })
        .collect()
}

impl SecretsFile {
    /// 解析；不合 Node 的 schema 时为 None。
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(bytes) else {
            return None;
        };
        if object.get("version").and_then(Value::as_u64) != Some(SECRETS_VERSION)
            || object
                .keys()
                .any(|key| !SECRETS_KEYS.contains(&key.as_str()))
        {
            return None;
        }
        let user = object
            .get("user")
            .filter(|v| valid_layer(v))?
            .as_object()?
            .clone();
        let workspaces = object.get("workspaces")?.as_object()?.clone();
        workspaces
            .values()
            .all(valid_layer)
            .then_some(Self { user, workspaces })
    }

    /// 读侧的视图（值是秘密类型）。
    pub fn redacted(&self) -> PluginSecrets {
        PluginSecrets {
            user: redact(&self.user),
            workspaces: self
                .workspaces
                .iter()
                .filter_map(|(root, layer)| Some((root.clone(), redact(layer.as_object()?))))
                .collect(),
        }
    }

    /// 这一层这个插件现在的值（同一个工作区键的多份写法合并，Node `getPluginSecretsForScope`）。
    fn current(&self, scope: &Scope, id: &str) -> Map<String, Value> {
        let layers: Vec<&Map<String, Value>> = match scope {
            Scope::User => vec![&self.user],
            Scope::Workspace(key) => self
                .workspaces
                .iter()
                .filter(|(root, _)| project_key(root) == *key)
                .filter_map(|(_, layer)| layer.as_object())
                .collect(),
        };
        let mut merged = Map::new();
        for layer in layers {
            if let Some(Value::Object(values)) = layer.get(id) {
                merged.extend(values.clone());
            }
        }
        merged
    }

    /// 写一个插件在一层的值：先删 `clear`，再写 `values`（Node `setPluginSecrets`）。工作区层的旧写法并进这一个键。
    pub fn set(&mut self, scope: &Scope, id: &str, values: &Map<String, Value>, clear: &[String]) {
        let mut next: Map<String, Value> = self
            .current(scope, id)
            .into_iter()
            .filter(|(key, _)| !clear.contains(key))
            .collect();
        next.extend(values.clone());
        match scope {
            Scope::User => {
                self.user.insert(id.to_owned(), Value::Object(next));
            }
            Scope::Workspace(key) => {
                let mut merged = Map::new();
                let roots: Vec<String> = self
                    .workspaces
                    .keys()
                    .filter(|root| project_key(root) == *key)
                    .cloned()
                    .collect();
                for root in roots {
                    if let Some(Value::Object(layer)) = self.workspaces.shift_remove(&root) {
                        merged.extend(layer);
                    }
                }
                merged.insert(id.to_owned(), Value::Object(next));
                self.workspaces.insert(key.clone(), Value::Object(merged));
            }
        }
    }

    /// 删一个插件的值：一层，或全部层（None，卸载用；Node `removePluginSecrets`）。
    pub fn remove(&mut self, id: &str, scope: Option<&Scope>) {
        match scope {
            None => {
                self.user.shift_remove(id);
                for layer in self.workspaces.values_mut() {
                    if let Value::Object(layer) = layer {
                        layer.shift_remove(id);
                    }
                }
            }
            Some(Scope::User) => {
                self.user.shift_remove(id);
            }
            Some(Scope::Workspace(key)) => {
                for (root, layer) in &mut self.workspaces {
                    if project_key(root) == *key
                        && let Value::Object(layer) = layer
                    {
                        layer.shift_remove(id);
                    }
                }
            }
        }
    }

    /// 写出的文字：去掉空的插件与空的工作区（Node `prunePluginSecretsFile`），两格缩进加换行。
    pub fn to_text(&self) -> String {
        let prune = |layer: &Map<String, Value>| -> Map<String, Value> {
            layer
                .iter()
                .filter(|(_, values)| values.as_object().is_some_and(|v| !v.is_empty()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let workspaces: Map<String, Value> = self
            .workspaces
            .iter()
            .filter_map(|(root, layer)| {
                let layer = prune(layer.as_object()?);
                (!layer.is_empty()).then(|| (root.clone(), Value::Object(layer)))
            })
            .collect();
        super::json_text(&json!({
            "version": SECRETS_VERSION,
            "user": Value::Object(prune(&self.user)),
            "workspaces": Value::Object(workspaces),
        }))
    }
}

#[cfg(test)]
#[path = "secrets_tests.rs"]
mod tests;
