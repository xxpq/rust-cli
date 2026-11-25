//! 一次加载的结果（docs/specs/rust-cli/ext.md §4.0）：不可变，内核只持它；id 是内容哈希。

use rust_cli_base::plugin::PluginContext;
use serde_json::{Value, json};

use super::entries::PluginEntry;
use super::{Plugin, PluginCommandRoot, Skipped};
use crate::mcp::McpServer;
use crate::skills::PluginSkillRoot;

/// 加载错误的种类（ext.md §4.14 的表；名字是一个开放集合里的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginErrorKind {
    PathNotFound,
    ManifestValidation,
    McpConfigInvalid,
    Generic,
}

impl PluginErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathNotFound => "path-not-found",
            Self::ManifestValidation => "manifest-validation-error",
            Self::McpConfigInvalid => "mcp-config-invalid",
            Self::Generic => "generic-error",
        }
    }
}

/// 一条加载错误（headless 的 `plugin_errors`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError {
    /// 插件 id；还没读出 id 时是 `inline[N]`（`--plugin-dir`）或目录。
    pub plugin: String,
    pub kind: PluginErrorKind,
    pub message: String,
    /// 整件没加载的 `--plugin-dir` 的目录。
    pub path: Option<String>,
}

impl PluginError {
    pub(crate) fn of(context: &PluginContext, kind: PluginErrorKind, message: String) -> Self {
        tracing::warn!(
            plugin = context.id,
            kind = kind.as_str(),
            message,
            "plugin load error"
        );
        Self {
            plugin: context.id.clone(),
            kind,
            message,
            path: None,
        }
    }
}

/// 加载好的插件、跳过的候选与加载错误。
#[derive(Debug, Clone, PartialEq)]
pub struct PluginSet {
    id: String,
    plugins: Vec<Plugin>,
    skipped: Vec<Skipped>,
    errors: Vec<PluginError>,
}

impl Default for PluginSet {
    /// 没有插件（不加载插件的组装与测试）：id 同样是内容哈希。
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new(), Vec::new())
    }
}

impl PluginSet {
    pub fn new(plugins: Vec<Plugin>, skipped: Vec<Skipped>, errors: Vec<PluginError>) -> Self {
        Self {
            id: digest(&plugins),
            plugins,
            skipped,
            errors,
        }
    }

    /// 内容哈希（SHA-256，hex）：同一份输入得到同一个 id（ext.md §4.0）。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 加载好的插件，按加载顺序。
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    pub fn errors(&self) -> &[PluginError] {
        &self.errors
    }

    /// 名字是 `name` 的插件（加载好的插件名字唯一，§4.3 第 7、8 步）。
    pub fn find(&self, name: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|plugin| plugin.name() == name)
    }

    pub fn skill_roots(&self) -> Vec<PluginSkillRoot> {
        self.plugins.iter().flat_map(Plugin::skill_roots).collect()
    }

    /// 全部插件的命令根，带插件名，按插件顺序（§4.5.1）。
    pub fn command_roots(&self) -> Vec<(String, PluginCommandRoot)> {
        self.plugins
            .iter()
            .flat_map(|plugin| {
                plugin
                    .command_roots
                    .iter()
                    .map(|root| (plugin.name().to_owned(), root.clone()))
            })
            .collect()
    }

    pub fn mcp_servers(&self) -> Vec<McpServer> {
        self.plugins
            .iter()
            .flat_map(|plugin| plugin.mcp_servers.iter().cloned())
            .collect()
    }

    /// 插件引用目录的条目（§4.7）。
    pub fn entries(&self) -> Vec<PluginEntry> {
        super::entries::entries(self)
    }
}

/// 每个加载好的插件的 id、目录、数据目录、manifest 与配置值（敏感值也进摘要，不进任何文字），按加载顺序。
fn digest(plugins: &[Plugin]) -> String {
    let content: Vec<Value> = plugins
        .iter()
        .map(|plugin| {
            let context = &plugin.context;
            let options: serde_json::Map<String, Value> = context
                .options
                .iter()
                .map(|(key, value)| (key.clone(), Value::from(value.expose())))
                .collect();
            json!({
                "id": context.id,
                "root": context.root,
                "data": context.data,
                "manifest": plugin.manifest,
                "options": options,
            })
        })
        .collect();
    rust_cli_base::hash::sha256_hex(Value::Array(content).to_string().as_bytes())
}
