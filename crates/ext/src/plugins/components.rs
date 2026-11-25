//! 一个插件的组件（docs/specs/rust-cli/ext.md §4.5、§4.10）：skill 根、命令根、agent 文件、hooks 与 MCP 服务器。
//!
//! 路径规则：skill 追加（缺省目录在就加载，manifest 的接在后面；都没有时插件根的 `SKILL.md` 算一个 skill）；命令与 agent
//! 写了 manifest 就替代缺省目录；hooks 追加；MCP 同名时 manifest 的覆盖 `.mcp.json` 的。越出插件目录的路径不读。

use std::collections::BTreeMap;

use rust_cli_base::names::vars;
use rust_cli_base::plugin::{
    OptionError, PluginContext, PluginOption, PluginOptions, expand_any_options,
    expand_plain_options,
};
use rust_cli_kernel_api::env_files::FileKind;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::{Map, Value};

use super::set::{PluginError, PluginErrorKind};
use super::{PluginAgentFile, Reader, inside, join, paths};
use crate::mcp::{McpScope, McpServer, parse_servers};

/// 缺省的组件目录与文件。
const SKILLS_DIR: &str = "skills";
const COMMANDS_DIR: &str = "commands";
const AGENTS_DIR: &str = "agents";
const HOOKS_FILE: &str = "hooks/hooks.json";
const MCP_FILE: &str = ".mcp.json";
/// 插件根自己是一个 skill 时的文件（§4.5 的 skill 行）。
const SKILL_FILE: &str = "SKILL.md";
const MARKDOWN_SUFFIX: &str = ".md";
/// manifest 的组件字段。
const SKILLS_KEY: &str = "skills";
const COMMANDS_KEY: &str = "commands";
const AGENTS_KEY: &str = "agents";
const HOOKS_KEY: &str = "hooks";
const MCP_KEY: &str = "mcpServers";
const USER_CONFIG_KEY: &str = "userConfig";
/// `userConfig` 里一项的敏感标记（ext.md §4.9）。
use rust_cli_base::plugin::SENSITIVE_KEY;
/// 不运行、只记 info 的组件字段（Node `UNSUPPORTED_COMPONENT_KEYS`，另加两种别的插件格式的字段）。
const DIAGNOSTIC_ONLY_KEYS: [&str; 6] = [
    "outputStyles",
    "lspServers",
    "channels",
    "settings",
    "themes",
    "monitors",
];

/// 命令对象映射里盖过 frontmatter 的字段（§4.5.1）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandMetadata {
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub model: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
}

/// 插件的一个命令根（§4.5.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCommandRoot {
    /// 目录：同 §2.10 的递归规则。
    Dir(String),
    /// 单个 `.md`：名字是 `name`（对象映射的键）或文件名去掉 `.md`。
    File {
        path: String,
        name: Option<String>,
        metadata: CommandMetadata,
    },
}

/// 读组件时的输出：组件与错误。
pub(super) struct Components {
    pub(super) skill_dirs: Vec<String>,
    pub(super) command_roots: Vec<PluginCommandRoot>,
    pub(super) agent_files: Vec<PluginAgentFile>,
    pub(super) hooks: Vec<Value>,
    pub(super) mcp_servers: Vec<McpServer>,
}

impl Reader<'_> {
    /// manifest 列出的一个路径；越出插件目录或不存在时记错误。
    async fn declared(
        &self,
        context: &PluginContext,
        relative: &str,
        errors: &mut Vec<PluginError>,
    ) -> Option<(String, FileKind)> {
        let Some(path) = inside(&context.root, relative) else {
            errors.push(PluginError::of(
                context,
                PluginErrorKind::PathNotFound,
                format!("component path escapes the plugin root: {relative}"),
            ));
            return None;
        };
        match self.kind(&path).await {
            Some(kind) => Some((path, kind)),
            None => {
                errors.push(PluginError::of(
                    context,
                    PluginErrorKind::PathNotFound,
                    format!("component path specified in the manifest was not found: {relative}"),
                ));
                None
            }
        }
    }

    /// 缺省目录存在时；manifest 也写了这个组件时不读它、记一条 info（ext.md §4.5 的「替代」）。
    async fn default_dir(
        &self,
        context: &PluginContext,
        dir: &str,
        key: &str,
        manifest: &Value,
    ) -> Option<String> {
        let path = join(&context.root, dir);
        if self.kind(&path).await != Some(FileKind::Directory) {
            return None;
        }
        if manifest.get(key).is_some() {
            tracing::info!(
                plugin = context.id,
                folder = path,
                field = key,
                "the default folder is not loaded because the manifest sets the field"
            );
            return None;
        }
        Some(path)
    }

    async fn skill_dirs(
        &self,
        context: &PluginContext,
        manifest: &Value,
        errors: &mut Vec<PluginError>,
    ) -> Vec<String> {
        let default = join(&context.root, SKILLS_DIR);
        let mut dirs = Vec::new();
        if self.kind(&default).await == Some(FileKind::Directory) {
            dirs.push(default.clone());
        }
        let declared = manifest.get(SKILLS_KEY);
        for relative in paths(declared) {
            let Some((path, kind)) = self.declared(context, &relative, errors).await else {
                continue;
            };
            if kind == FileKind::Directory && !dirs.contains(&path) {
                dirs.push(path);
            }
        }
        // 都没有时插件根的 `SKILL.md` 让插件根本身成为一个 skill。
        if declared.is_none()
            && dirs.is_empty()
            && self.kind(&join(&context.root, SKILL_FILE)).await == Some(FileKind::File)
        {
            dirs.push(context.root.clone());
        }
        dirs
    }

    async fn command_roots(
        &self,
        context: &PluginContext,
        manifest: &Value,
        errors: &mut Vec<PluginError>,
    ) -> Vec<PluginCommandRoot> {
        if let Some(dir) = self
            .default_dir(context, COMMANDS_DIR, COMMANDS_KEY, manifest)
            .await
        {
            return vec![PluginCommandRoot::Dir(dir)];
        }
        let mut roots = Vec::new();
        match manifest.get(COMMANDS_KEY) {
            Some(Value::Object(mapping)) => {
                for (name, entry) in mapping {
                    let Some(source) = entry.get("source").and_then(Value::as_str) else {
                        errors.push(PluginError::of(
                            context,
                            PluginErrorKind::Generic,
                            format!(
                                "command \"{name}\" has no source; inline content is not supported"
                            ),
                        ));
                        continue;
                    };
                    if let Some((path, FileKind::File)) =
                        self.declared(context, source, errors).await
                    {
                        roots.push(PluginCommandRoot::File {
                            path,
                            name: Some(name.clone()),
                            metadata: metadata(entry),
                        });
                    }
                }
            }
            other => {
                for relative in paths(other) {
                    match self.declared(context, &relative, errors).await {
                        Some((path, FileKind::Directory)) => {
                            roots.push(PluginCommandRoot::Dir(path))
                        }
                        Some((path, FileKind::File)) if path.ends_with(MARKDOWN_SUFFIX) => {
                            roots.push(PluginCommandRoot::File {
                                path,
                                name: None,
                                metadata: CommandMetadata::default(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        roots
    }

    async fn agent_files(
        &self,
        context: &PluginContext,
        manifest: &Value,
        errors: &mut Vec<PluginError>,
    ) -> Vec<PluginAgentFile> {
        if let Some(dir) = self
            .default_dir(context, AGENTS_DIR, AGENTS_KEY, manifest)
            .await
        {
            return self.markdown(&dir).await;
        }
        let mut files: Vec<PluginAgentFile> = Vec::new();
        for relative in paths(manifest.get(AGENTS_KEY)) {
            let found = match self.declared(context, &relative, errors).await {
                Some((path, FileKind::Directory)) => self.markdown(&path).await,
                Some((path, FileKind::File)) if path.to_lowercase().ends_with(MARKDOWN_SUFFIX) => {
                    vec![PluginAgentFile {
                        path,
                        namespace: Vec::new(),
                    }]
                }
                _ => Vec::new(),
            };
            // 同一文件只取一次。
            for file in found {
                if !files.iter().any(|known| known.path == file.path) {
                    files.push(file);
                }
            }
        }
        files
    }

    async fn hook_files(&self, context: &PluginContext, manifest: &Value) -> Vec<Value> {
        let mut hooks = Vec::new();
        let default = join(&context.root, HOOKS_FILE);
        if self.kind(&default).await == Some(FileKind::File)
            && let Some(file) = self.json(&default).await
        {
            hooks.push(file);
        }
        match manifest.get(HOOKS_KEY) {
            // 内联对象是事件表，包成文件的形状（`hooks` 字段）。
            Some(Value::Object(inline)) => {
                hooks.push(serde_json::json!({ HOOKS_KEY: Value::Object(inline.clone()) }));
            }
            other => {
                for relative in paths(other) {
                    if let Some(path) = inside(&context.root, &relative) {
                        hooks.extend(self.json(&path).await);
                    }
                }
            }
        }
        hooks
    }

    /// MCP 服务器声明：`.mcp.json` 与 manifest 的 `mcpServers`（内联对象，或指向 JSON 文件的路径）合成一张表，同名时后者
    /// 覆盖（Node `{...fromFile, ...fromManifest}`）。每张表去掉外层的 `mcpServers`。
    pub(super) async fn mcp_table(&self, dir: &str, manifest: &Value) -> Map<String, Value> {
        let mut tables = Vec::new();
        let default = join(dir, MCP_FILE);
        if self.kind(&default).await == Some(FileKind::File)
            && let Some(file) = self.json(&default).await
        {
            tables.push(file);
        }
        match manifest.get(MCP_KEY) {
            Some(Value::Object(inline)) => tables.push(Value::Object(inline.clone())),
            other => {
                for relative in paths(other) {
                    if let Some(path) = inside(dir, &relative)
                        && let Some(file) = self.json(&path).await
                    {
                        tables.push(file);
                    }
                }
            }
        }
        let mut merged = Map::new();
        for table in tables {
            let table = table.get(MCP_KEY).cloned().unwrap_or(table);
            if let Value::Object(servers) = table {
                merged.extend(servers);
            }
        }
        merged
    }

    /// 插件的组件（§4.5）。`table` 是 [`Self::mcp_table`] 的结果。
    pub(super) async fn components(
        &self,
        context: &PluginContext,
        manifest: &Value,
        table: Map<String, Value>,
        errors: &mut Vec<PluginError>,
    ) -> Components {
        for key in DIAGNOSTIC_ONLY_KEYS {
            if manifest.get(key).is_some() {
                tracing::info!(
                    plugin = context.id,
                    component = key,
                    "plugin component is diagnostic-only"
                );
            }
        }
        // 四类组件同时读（ext.md §4.0）；各自的错误按 skill、命令、agent 的顺序并入，同以前逐个读时。
        let (mut skill_errors, mut command_errors, mut agent_errors) =
            (Vec::new(), Vec::new(), Vec::new());
        let (skill_dirs, command_roots, agent_files, hooks) = futures_util::join!(
            self.skill_dirs(context, manifest, &mut skill_errors),
            self.command_roots(context, manifest, &mut command_errors),
            self.agent_files(context, manifest, &mut agent_errors),
            self.hook_files(context, manifest),
        );
        errors.extend(skill_errors);
        errors.extend(command_errors);
        errors.extend(agent_errors);
        Components {
            skill_dirs,
            command_roots,
            agent_files,
            hooks,
            mcp_servers: mcp_servers(
                context,
                self.workspace.root.as_str(),
                &mcp_options(context, manifest),
                table,
                errors,
            ),
        }
    }
}

fn text(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn metadata(entry: &Value) -> CommandMetadata {
    CommandMetadata {
        description: text(entry, "description"),
        argument_hint: text(entry, "argumentHint"),
        model: text(entry, "model"),
        allowed_tools: entry
            .get("allowedTools")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            }),
    }
}

/// manifest 的 `userConfig`（对象；别的形状当没写）。
pub(super) fn user_config(manifest: &Value) -> Option<&Map<String, Value>> {
    manifest.get(USER_CONFIG_KEY).and_then(Value::as_object)
}

/// 存下的配置值按 manifest 标出敏感键（§4.9）：标了 `sensitive: true` 的键，明文层里的值也换成秘密。
pub(super) fn mark_sensitive(mut options: PluginOptions, manifest: &Value) -> PluginOptions {
    let Some(declared) = user_config(manifest) else {
        return options;
    };
    for (key, option) in declared {
        if option.get(SENSITIVE_KEY).and_then(Value::as_bool) != Some(true) {
            continue;
        }
        if let Some(PluginOption::Plain(value)) = options.get(key) {
            let secret = rust_cli_base::secret::RedactedString::new(value.clone());
            options.insert(key.clone(), PluginOption::Secret(secret));
        }
    }
    options
}

/// MCP 里 `${user_config.*}` 用的值：存下的值，加上 manifest 的缺省（声明了 `default` 用它；没写也不是 `required` 的是
/// 空串；`required` 又没有值的没有）。manifest 没声明 `userConfig` 时为 None：不展开（§4.10）。
fn mcp_options(context: &PluginContext, manifest: &Value) -> Option<PluginOptions> {
    let declared = user_config(manifest)?;
    let mut options = context.options.clone();
    for (key, option) in declared {
        if options.contains_key(key) {
            continue;
        }
        let required = option.get("required").and_then(Value::as_bool) == Some(true);
        let default = match option.get("default") {
            Some(value) => PluginOption::scalar(value),
            None => (!required).then(String::new),
        };
        let Some(default) = default else {
            continue;
        };
        let sensitive = option.get(SENSITIVE_KEY).and_then(Value::as_bool) == Some(true);
        let value = if sensitive {
            PluginOption::Secret(rust_cli_base::secret::RedactedString::new(default))
        } else {
            PluginOption::Plain(default)
        };
        options.insert(key.clone(), value);
    }
    Some(options)
}

/// MCP 服务器配置里要展开的字段：只能放非敏感值的与能放敏感值的（§4.10）。
const PLAIN_FIELDS: [&str; 3] = ["command", "cwd", "url"];
const PLAIN_LIST_FIELDS: [&str; 1] = ["args"];
const ANY_MAP_FIELDS: [&str; 2] = ["env", "headers"];

/// 一个服务器配置里的插件变量与 `${user_config.*}`；不让用的引用是错误（这个服务器不连）。
fn expand_server(
    context: &PluginContext,
    options: Option<&PluginOptions>,
    server: &mut Map<String, Value>,
) -> Result<(), OptionError> {
    let plain = |text: &str| -> Result<String, OptionError> {
        let text = context.expand_paths(text);
        match options {
            Some(options) => expand_plain_options(&text, options),
            None => Ok(text),
        }
    };
    let any = |text: &str| -> Result<String, OptionError> {
        let text = context.expand_paths(text);
        match options {
            Some(options) => expand_any_options(&text, options),
            None => Ok(text),
        }
    };
    for field in PLAIN_FIELDS {
        if let Some(Value::String(text)) = server.get_mut(field) {
            *text = plain(text)?;
        }
    }
    for field in PLAIN_LIST_FIELDS {
        if let Some(Value::Array(items)) = server.get_mut(field) {
            for item in items {
                if let Value::String(text) = item {
                    *text = plain(text)?;
                }
            }
        }
    }
    for field in ANY_MAP_FIELDS {
        if let Some(Value::Object(map)) = server.get_mut(field) {
            for value in map.values_mut() {
                if let Value::String(text) = value {
                    *text = any(text)?;
                }
            }
        }
    }
    Ok(())
}

/// `project` 是工作区根：stdio 服务器的环境按 Node（`adapters/src/plugins/mcp.ts`）给插件目录、数据目录与项目目录，两套
/// 名字，配置里的 `env` 优先；`ZCODE_PLUGIN_ID` 总是插件的 id（配置写不了）。
fn mcp_servers(
    context: &PluginContext,
    project: &str,
    options: &Option<PluginOptions>,
    table: Map<String, Value>,
    errors: &mut Vec<PluginError>,
) -> Vec<McpServer> {
    let mut expanded = Map::new();
    for (name, mut server) in table {
        let Value::Object(fields) = &mut server else {
            continue;
        };
        match expand_server(context, options.as_ref(), fields) {
            Ok(()) => {
                expanded.insert(name, server);
            }
            Err(error) => errors.push(PluginError::of(
                context,
                PluginErrorKind::McpConfigInvalid,
                format!("MCP server \"{name}\" is skipped: {error}"),
            )),
        }
    }
    // 插件变量已经换掉；剩下的 `${VAR}` 按 §3.1 的规则（插件的服务器没有别的变量来源）。
    let none = |_: &str| None;
    let (found, warnings) = parse_servers(&Value::Object(expanded), McpScope::Plugin, &none);
    for warning in warnings {
        tracing::warn!(plugin = context.id, warning, "plugin MCP configuration");
    }
    let mut provided: BTreeMap<&str, &str> = BTreeMap::from([
        (vars::CLAUDE_PLUGIN_ROOT, context.root.as_str()),
        (vars::ZCODE_PLUGIN_ROOT, context.root.as_str()),
        (vars::CLAUDE_PROJECT_DIR, project),
        (vars::ZCODE_PROJECT_DIR, project),
    ]);
    if let Some(data) = &context.data {
        provided.insert(vars::CLAUDE_PLUGIN_DATA, data);
        provided.insert(vars::ZCODE_PLUGIN_DATA, data);
    }
    found
        .into_iter()
        .map(|mut server| {
            if let McpTransportConfig::Stdio { env, .. } = &mut server.transport {
                for (name, value) in &provided {
                    env.entry((*name).to_owned())
                        .or_insert_with(|| (*value).to_owned());
                }
                // 插件身份由加载结果写，manifest 的 env 改不了（Node 同）。
                env.insert(vars::ZCODE_PLUGIN_ID.to_owned(), context.id.clone());
            }
            McpServer {
                name: format!("plugin:{}:{}", context.name, server.name),
                ..server
            }
        })
        .collect()
}
