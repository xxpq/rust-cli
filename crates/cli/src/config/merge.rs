//! 三段配置的合并（docs/specs/rust-cli/config.md §2；Node `config-merger.ts`、`config-factory.ts`）。
//!
//! 输入是按优先级排好的层（用户级在前，项目级自根向工作区根，最后是 `--settings`），都已是 JSON 对象；这里不做 IO。项目级的层
//! 读文件时已经只剩扩展声明（`files::project_scope`，config.md §1.1），所以下面各段的合并不按层区分（MCP 的作用域除外）。

use rust_cli_base::ui_prefs::UiLocale;
use rust_cli_base::units;
use rust_cli_ext::mcp::{
    McpScope, McpServer, NodeMcpServer, anchor_project_cwd, config_dir, parse_node_servers,
};
use rust_cli_kernel_api::permission::auto_mode::{AutoModeRules, AutoModeSettings};
use rust_cli_kernel_api::permission::{PermissionMode, RuleSource};
use rust_cli_platform::local::{ConfigFile, McpTable};
use serde_json::{Map, Value};

use super::files::AgentsMcp;

/// 一层配置：来源与对象。
#[derive(Clone, Copy)]
pub(crate) struct Layer<'a> {
    pub(crate) source: RuleSource,
    pub(crate) object: &'a Map<String, Value>,
    /// 警告里标出的文件。
    pub(crate) path: &'a str,
}

/// 合并后的 `permission`：每个键取最后写它的层。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Permission {
    pub(crate) mode: Option<PermissionMode>,
    pub(crate) allowed: Option<(RuleSource, Vec<String>)>,
    pub(crate) disallowed: Option<(RuleSource, Vec<String>)>,
    /// 最后写 `allowedTools` / `disallowedTools` 的那一层的文件（设置的 Permissions 段打开它，tui-manage.md §1.2）。
    pub(crate) allowed_file: Option<String>,
    pub(crate) disallowed_file: Option<String>,
    /// 自动模式的设置（docs/specs/rust-cli/auto-classifier.md §16.6）。
    pub(crate) auto_mode: AutoModeSettings,
}

/// `permission.disableAutoMode` 关掉自动模式的写法。
const DISABLE_AUTO_MODE: &str = "disable";
/// 设置里的一组规则。
type Group = fn(&mut AutoModeRules) -> &mut Vec<String>;

/// `permission.autoMode` 的四组规则：键名与设置里的哪一组。
const AUTO_MODE_GROUPS: [(&str, Group); 4] = [
    ("allow", |rules| &mut rules.allow),
    ("softDeny", |rules| &mut rules.soft_deny),
    ("hardDeny", |rules| &mut rules.hard_deny),
    ("environment", |rules| &mut rules.environment),
];

/// 一层的自动模式设置并进 `merged`：plan 的自动语义与 `disableAutoMode` 任一层关就关，`classifyAllShell` 任一层开就开，规则各层拼接
/// （照对照实现的合并）。类型不对的值记警告、跳过。
fn auto_mode(
    merged: &mut AutoModeSettings,
    section: &Map<String, Value>,
    path: &str,
    warnings: &mut Vec<String>,
) {
    if let Some(value) = section.get("useAutoModeDuringPlan") {
        match value.as_bool() {
            Some(enabled) => merged.use_auto_mode_during_plan &= enabled,
            None => warnings.push(format!(
                "{path}: permission.useAutoModeDuringPlan must be a boolean"
            )),
        }
    }
    if let Some(value) = section.get("disableAutoMode") {
        match value.as_str() {
            Some(DISABLE_AUTO_MODE) => merged.disabled = true,
            _ => warnings.push(format!(
                "{path}: permission.disableAutoMode must be \"{DISABLE_AUTO_MODE}\""
            )),
        }
    }
    let Some(section) = section.get("autoMode") else {
        return;
    };
    let Some(section) = section.as_object() else {
        warnings.push(format!("{path}: permission.autoMode must be an object"));
        return;
    };
    for (key, group) in AUTO_MODE_GROUPS {
        if let Some(value) = section.get(key) {
            match strings(value) {
                Some(items) => group(&mut merged.rules).extend(items),
                None => warnings.push(format!(
                    "{path}: permission.autoMode.{key} must be an array of strings"
                )),
            }
        }
    }
    if let Some(value) = section.get("classifyAllShell") {
        match value.as_bool() {
            Some(all) => merged.classify_all_shell |= all,
            None => warnings.push(format!(
                "{path}: permission.autoMode.classifyAllShell must be a boolean"
            )),
        }
    }
}

/// Node 的模式值 → 权限模式（config.md §2）。修复线 F20：`guarded` 是 guarded（以前当 bypassPermissions）；
/// `plan` 是单值的 plan（新会话 plan 开、default）。
pub(crate) fn mode_of(value: &str) -> Option<PermissionMode> {
    PermissionMode::from_node_name(value)
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

pub(crate) fn permission(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Permission {
    let mut merged = Permission::default();
    for layer in layers {
        let Some(section) = layer.object.get("permission") else {
            continue;
        };
        let Some(section) = section.as_object() else {
            warnings.push(format!("{}: permission must be an object", layer.path));
            continue;
        };
        auto_mode(&mut merged.auto_mode, section, layer.path, warnings);
        if let Some(mode) = section.get("mode") {
            match mode.as_str().and_then(mode_of) {
                Some(mode) => merged.mode = Some(mode),
                None => warnings.push(format!("{}: unknown permission.mode {mode}", layer.path)),
            }
        }
        for (key, slot, file) in [
            (
                "allowedTools",
                &mut merged.allowed,
                &mut merged.allowed_file,
            ),
            (
                "disallowedTools",
                &mut merged.disallowed,
                &mut merged.disallowed_file,
            ),
        ] {
            if let Some(value) = section.get(key) {
                match strings(value) {
                    Some(rules) => {
                        *slot = Some((layer.source, rules));
                        *file = Some(layer.path.to_owned());
                    }
                    None => warnings.push(format!(
                        "{}: permission.{key} must be an array of strings",
                        layer.path
                    )),
                }
            }
        }
    }
    merged
}

/// `ui.locale` 按键覆盖（tui.md §7.1，Node `ui.locale`）：只认 `zh-CN`、`en-US`、`auto`，坏值记警告、
/// 跳过这一层。
pub(crate) fn locale(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Option<UiLocale> {
    let mut merged = None;
    for layer in layers {
        let Some(value) = layer.object.get("ui").and_then(|ui| ui.get("locale")) else {
            continue;
        };
        match value.as_str().and_then(UiLocale::parse) {
            Some(locale) => merged = Some(locale),
            None => warnings.push(format!("{}: unknown ui.locale {value}", layer.path)),
        }
    }
    merged
}

/// 顶层 `checkForUpdateOnStartup` 按键覆盖（自动更新的开关，docs/specs/rust-cli/update.md §10，F48）：只认布尔，
/// 坏值记警告、跳过这一层；没有时开。
pub(crate) fn check_for_update(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> bool {
    let mut merged = true;
    for layer in layers {
        let Some(value) = layer.object.get("checkForUpdateOnStartup") else {
            continue;
        };
        match value.as_bool() {
            Some(enabled) => merged = enabled,
            None => warnings.push(format!(
                "{}: checkForUpdateOnStartup must be a boolean",
                layer.path
            )),
        }
    }
    merged
}

/// `askUserQuestionTimeout` 的三个取值 `60s`、`5m`、`10m` 的毫秒数。
const QUESTION_TIMEOUT_60S_MS: u64 = 60 * units::MS_PER_SECOND;
const QUESTION_TIMEOUT_5M_MS: u64 = 5 * units::MS_PER_MINUTE;
const QUESTION_TIMEOUT_10M_MS: u64 = 10 * units::MS_PER_MINUTE;

/// 顶层 `askUserQuestionTimeout` 按键覆盖（提问框的空闲超时，docs/specs/rust-cli/tools-more.md §1.1，工作线 F72）：只认
/// `60s`、`5m`、`10m`、`never`，坏值记警告、跳过这一层；没有或 `never` 时不计时（None）。返回毫秒。
pub(crate) fn question_timeout(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Option<u64> {
    let mut merged = None;
    for layer in layers {
        let Some(value) = layer.object.get("askUserQuestionTimeout") else {
            continue;
        };
        match value.as_str() {
            Some("60s") => merged = Some(QUESTION_TIMEOUT_60S_MS),
            Some("5m") => merged = Some(QUESTION_TIMEOUT_5M_MS),
            Some("10m") => merged = Some(QUESTION_TIMEOUT_10M_MS),
            Some("never") => merged = None,
            _ => warnings.push(format!(
                "{}: unknown askUserQuestionTimeout {value}",
                layer.path
            )),
        }
    }
    merged
}

/// `compact.autoThresholdTokens` 按键覆盖（自动压缩的位置，docs/specs/rust-cli/compact.md §2.3；Puffin 的 `/autocompact`
/// 与设置页的 Auto-compact at）：正整数才算，坏值记警告、跳过这一层；没有时 None（用模型的阈值）。另给定了合并后的值的
/// 文件，当它不是用户级文件时（写回只写用户级文件，改不了这个值）。
pub(crate) fn compact_at(
    layers: &[Layer<'_>],
    warnings: &mut Vec<String>,
) -> (Option<u64>, Option<String>) {
    let mut merged = None;
    let mut pinned = None;
    for layer in layers {
        let Some(value) = layer
            .object
            .get("compact")
            .and_then(|compact| compact.get("autoThresholdTokens"))
        else {
            continue;
        };
        match value.as_u64().filter(|tokens| *tokens > 0) {
            Some(tokens) => {
                merged = Some(tokens);
                pinned = (layer.source != RuleSource::UserSettings).then(|| layer.path.to_owned());
            }
            None => warnings.push(format!(
                "{}: unknown compact.autoThresholdTokens {value}",
                layer.path
            )),
        }
    }
    (merged, pinned)
}

/// `features.mcp` 按键覆盖；最终为 `false` 时关闭 MCP。
pub(crate) fn mcp_enabled(layers: &[Layer<'_>]) -> bool {
    layers
        .iter()
        .rev()
        .find_map(|layer| layer.object.get("features")?.get("mcp")?.as_bool())
        .unwrap_or(true)
}

/// MCP 服务器最终生效的定义所在的配置文件（`/mcp enable/disable` 写这里，tui-manage.md §6.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpOrigin {
    pub(crate) path: String,
    /// 用户级文件（经 platform 写）；否则是项目级文件（经执行环境写）。
    pub(crate) user: bool,
    /// 这个文件里的服务器表。
    pub(crate) table: McpTable,
}

/// 回落文件（docs/specs/rust-cli/ext.md §3.1）：工作区根的 `.agents/mcp.json`（项目级配置一个条目都没写时用）与 home 下的
/// `~/.agents/mcp.json`（用户级配置一个条目都没写时用）。读不出的配置文件挡住它那一级的回落，由调用方判断（`Files`）。
#[derive(Clone, Copy, Default)]
pub(crate) struct Fallbacks<'a> {
    pub(crate) project: Option<&'a AgentsMcp>,
    pub(crate) user: Option<&'a AgentsMcp>,
}

/// 合并后的 `mcp.servers`：启用的、停用的（`enabled: false`），以及每个名字的来源文件。
#[derive(Debug, Clone, Default)]
pub(crate) struct McpLayers {
    pub(crate) servers: Vec<McpServer>,
    pub(crate) disabled: Vec<McpServer>,
    pub(crate) origins: std::collections::BTreeMap<String, McpOrigin>,
}

/// `mcp.servers`：项目级按名合并（后面的覆盖前面的），然后用户级按名覆盖项目级（Node
/// `resolveEffectiveMcpServers`：用户级遮住项目级），最后去掉停用的。结果按名字排序。
#[cfg(test)]
pub(crate) fn mcp_servers(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Vec<McpServer> {
    mcp_layers(layers, Fallbacks::default(), warnings).servers
}

/// 一层配置文件的服务器（警告带上文件）；项目级的工作目录按那一层的目录解析（config.md §2.7），用户级的照旧由执行环境
/// 相对工作区根解析。
fn layer_servers(layer: &Layer<'_>, warnings: &mut Vec<String>) -> Vec<NodeMcpServer> {
    let table = McpTable::Config;
    let Some(value) = table.of(layer.object) else {
        return Vec::new();
    };
    let scope = match layer.source {
        RuleSource::UserSettings => McpScope::User,
        _ => McpScope::Project,
    };
    let (mut servers, found) = parse_node_servers(value, scope, &table.name());
    warnings.extend(found.into_iter().map(|w| format!("{}: {w}", layer.path)));
    if scope == McpScope::Project {
        let base = config_dir(layer.path);
        for found in &mut servers {
            anchor_project_cwd(&mut found.server, &base);
        }
    }
    servers
}

/// 这一层一个 MCP 条目都没写：`mcp`、`mcp.servers` 没写，或表是空对象。`mcp` 或表不是对象算写了（Node：前者整份文件作废，
/// 后者 `config_mcp_server_invalid`）。条目不是合法就是不合法，所以这就是 Node 回落的条件“没有合法的、也没有不合法的”。
fn declares_no_servers(object: &Map<String, Value>) -> bool {
    let mut current = object;
    for key in McpTable::Config.path() {
        match current.get(*key) {
            None => return true,
            Some(Value::Object(next)) => current = next,
            Some(_) => return false,
        }
    }
    current.is_empty()
}

/// 回落文件的服务器（`scope`：工作区根的是项目级，home 下的是用户级）；相对的工作目录按 `agents.anchor` 锚定（项目级的按工作区根，
/// 用户级的不锚定）。读不出、表不是对象、坏的条目记警告，坏的条目不影响别的。
fn fallback_servers(
    agents: &AgentsMcp,
    scope: McpScope,
    warnings: &mut Vec<String>,
) -> Vec<NodeMcpServer> {
    if let ConfigFile::Invalid { reason } = &agents.file {
        warnings.push(format!("{}: the file is ignored: {reason}", agents.path));
    }
    let table = McpTable::Agents;
    let Some(value) = agents.file.object().and_then(|object| table.of(object)) else {
        return Vec::new();
    };
    let (mut servers, found) = parse_node_servers(value, scope, &table.name());
    warnings.extend(found.into_iter().map(|w| format!("{}: {w}", agents.path)));
    if let Some(base) = &agents.anchor {
        for found in &mut servers {
            anchor_project_cwd(&mut found.server, base);
        }
    }
    servers
}

/// 同 [`mcp_servers`]，另给停用的服务器与来源文件（停用的照样遮住低优先级的同名服务器）。回落（ext.md §3.1）：项目级各层一个条目
/// 都没写时，工作区根的 `.agents/mcp.json` 排在项目级的位置；用户级配置一个条目都没写时，`~/.agents/mcp.json` 排在最低（项目级同名的
/// 覆盖它：v4 App 去重时工作区来源优先，Node 的信任模块不认识这个文件，项目里同名的声明在那边仍是声明）。
pub(crate) fn mcp_layers(
    layers: &[Layer<'_>],
    fallbacks: Fallbacks<'_>,
    warnings: &mut Vec<String>,
) -> McpLayers {
    let mut merged = std::collections::BTreeMap::new();
    let mut origins = std::collections::BTreeMap::new();
    let mut add = |servers: Vec<NodeMcpServer>, origin: McpOrigin| {
        for found in servers {
            origins.insert(found.server.name.clone(), origin.clone());
            merged.insert(found.server.name.clone(), found);
        }
    };
    let project: Vec<&Layer<'_>> = layers
        .iter()
        .filter(|layer| layer.source == RuleSource::ProjectSettings)
        .collect();
    let user: Vec<&Layer<'_>> = layers
        .iter()
        .filter(|layer| layer.source == RuleSource::UserSettings)
        .collect();
    let user_declares_nothing = user.iter().all(|layer| declares_no_servers(layer.object));
    if let Some(agents) = fallbacks.user.filter(|_| user_declares_nothing) {
        let origin = McpOrigin {
            path: agents.path.clone(),
            user: true,
            table: McpTable::Agents,
        };
        add(fallback_servers(agents, McpScope::User, warnings), origin);
    }
    for layer in &project {
        let origin = McpOrigin {
            path: layer.path.to_owned(),
            user: false,
            table: McpTable::Config,
        };
        add(layer_servers(layer, warnings), origin);
    }
    let nothing_declared = project
        .iter()
        .all(|layer| declares_no_servers(layer.object));
    if let Some(agents) = fallbacks.project.filter(|_| nothing_declared) {
        let origin = McpOrigin {
            path: agents.path.clone(),
            user: false,
            table: McpTable::Agents,
        };
        add(
            fallback_servers(agents, McpScope::Project, warnings),
            origin,
        );
    }
    for layer in &user {
        let origin = McpOrigin {
            path: layer.path.to_owned(),
            user: true,
            table: McpTable::Config,
        };
        add(layer_servers(layer, warnings), origin);
    }
    // 停用的服务器在覆盖之后才分出来：用户级停用的服务器也遮住项目级的同名服务器。
    let (servers, disabled): (Vec<_>, Vec<_>) =
        merged.into_values().partition(|found| found.enabled);
    McpLayers {
        servers: servers.into_iter().map(|found| found.server).collect(),
        disabled: disabled.into_iter().map(|found| found.server).collect(),
        origins,
    }
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
