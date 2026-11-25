//! Node CLI 配置文件里的 `mcp.servers`（docs/specs/rust-cli/ext.md §3.1、config.md §2）。
//!
//! 外形是 Node 的 `McpServerConfig`（`apps/zcode-cli/packages/adapters/src/config/schema.ts`），解析前先做
//! Node 的旧格式规范化（`normalizeMcpServerConfigInput`）。值原样使用，不展开 `${VAR}`（Node 不展开）。
//! `cwd`、`timeoutMs`、`alwaysLoad` 按 Node 接上；`protocolVersion` 参与握手的协商（ext.md §3.10.2）；`oauth` 的
//! `authorization_code` 交给传输（ext.md §3.12.1），`client_credentials` 不做，记警告照常连接。

use std::collections::BTreeMap;

use rust_cli_base::names::DOT_DIR;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::mcp::{McpOAuth, McpTransportConfig};
use serde_json::{Map, Value};

use super::config::{McpIsolation, McpProtocol, McpScope, McpServer, McpServerOptions};
use super::connection::MODERN_REVISION;

/// 没写 `cwd` 时项目级的声明当作 `.`（相对它的基准目录）。
const CURRENT_DIR: &str = ".";

/// 项目配置文件里相对的 `cwd` 的基准目录（Node `getProjectConfigBaseDir`）：文件所在的目录，在 `.zcode/` 下时是 `.zcode` 的上一层。
/// 路径的写法按文件的路径推断。
pub fn config_dir(file: &str) -> String {
    let style = PathStyle::infer(file);
    let dir = style.dirname(file);
    if style.file_name(&dir) == DOT_DIR {
        style.dirname(&dir)
    } else {
        dir
    }
}

/// 项目级 stdio 服务器的工作目录：没写是 `.`；绝对路径原样；否则相对 `base`。`base` 由声明的来源定：项目配置文件是 [`config_dir`]
/// （Node `normalizeProjectMcpServer`），工作区根的 `.agents/mcp.json` 是工作区根（Node 按 `workingDirectory` 解析它）。
pub fn project_cwd(base: &str, cwd: Option<&str>) -> String {
    let style = PathStyle::infer(base);
    let cwd = cwd.unwrap_or(CURRENT_DIR);
    if style.is_absolute(cwd) {
        return cwd.to_owned();
    }
    style.join(base, &[cwd])
}

/// 项目级的 stdio 服务器：工作目录换成按 [`project_cwd`] 相对 `base` 解析好的绝对路径（所有形态一条规则）。
///
/// 缺陷修复（主会话审定 2026-11-05）：以前相对的 `cwd` 由执行环境一律按工作区根解析，而 Node 与信任记录的 digest 按声明它的那一层的
/// 目录解析。声明在祖先层（工作区是 git 根的子目录）时，用户信任的是一个目录，实际起进程的是另一个；仓库可以在那里放一个同名的
/// 可执行文件。修法：读配置时就按声明的基准目录解析，执行与审核看到同一个目录。
pub fn anchor_project_cwd(server: &mut McpServer, base: &str) {
    if let McpTransportConfig::Stdio { cwd, .. } = &mut server.transport {
        *cwd = Some(project_cwd(base, cwd.as_deref()));
    }
}

/// Node `normalizeMcpServerConfigInput`：旧字段名与缺省的 `type`。工作区声明的 digest 也从它的结果算（`declaration.rs`）。
pub(super) fn normalize(server: &Map<String, Value>) -> Map<String, Value> {
    let mut server = server.clone();
    if !server.contains_key("env")
        && let Some(environment) = server.get("environment").cloned()
    {
        server.insert("env".into(), environment);
    }
    server.remove("environment");
    let enable = server.get("enable").and_then(Value::as_bool);
    let enabled = server.get("enabled").and_then(Value::as_bool);
    if enable.is_some() || enabled.is_some() {
        // 冲突时以停用为准（Node 注释：桌面端写 enable:false 时不清理外部导入的 enabled:true）。
        let value = if enable == Some(false) {
            false
        } else {
            enabled.unwrap_or(true)
        };
        server.insert("enabled".into(), Value::Bool(value));
    }
    server.remove("enable");
    server.remove("timeout");
    server.remove("startup_timeout_sec");
    let kind = server
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match kind.as_deref() {
        Some("remote") => {
            server.insert("type".into(), "http".into());
        }
        Some(_) => {}
        None => {
            let present = |key: &str| {
                server
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
            };
            if present("command") {
                server.insert("type".into(), "stdio".into());
            } else if present("url") {
                server.insert("type".into(), "http".into());
            }
        }
    }
    let remote = matches!(
        server.get("type").and_then(Value::as_str),
        Some("http" | "sse")
    );
    if remote
        && !server.contains_key("headers")
        && let Some(headers) = server.get("http_headers").cloned()
    {
        server.insert("headers".into(), headers);
    }
    server.remove("http_headers");
    server
}

fn string_map(value: Option<&Value>, key: &str) -> Result<BTreeMap<String, String>, String> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Value::Object(fields)) => fields
            .iter()
            .map(|(name, value)| {
                value
                    .as_str()
                    .map(|value| (name.clone(), value.to_owned()))
                    .ok_or_else(|| format!("{key}.{name}: expected a string"))
            })
            .collect(),
        Some(_) => Err(format!("{key}: expected an object")),
    }
}

fn required(server: &Map<String, Value>, key: &str) -> Result<String, String> {
    server
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{key}: required"))
}

/// 配置文件里的一个服务器。停用的（`enabled: false`）也要给出：合并时它照样遮住低优先级的同名服务器，
/// 合并之后才去掉（Node 按名覆盖后再按 `enabled` 跳过连接）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeMcpServer {
    pub server: McpServer,
    pub enabled: bool,
}

/// `client_credentials` 的警告（ext.md §3.12.1、§3.12.10 第 1 项）。
const CLIENT_CREDENTIALS_UNSUPPORTED: &str =
    "oauth client_credentials is not supported; connecting without it";
/// `oauth.type` 的两个值（Node `mcpOAuthSchema`）。
const AUTHORIZATION_CODE: &str = "authorization_code";
const CLIENT_CREDENTIALS: &str = "client_credentials";

/// Node `mcpOAuthSchema` 的一个可选字段：有就要是非空字符串（`scope` 可以是空串）。
fn oauth_text(
    oauth: &Map<String, Value>,
    key: &str,
    empty: bool,
) -> Result<Option<String>, String> {
    match oauth.get(key) {
        None => Ok(None),
        Some(Value::String(text)) if empty || !text.is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(format!("oauth.{key}: expected a non-empty string")),
    }
}

/// Node `mcpOAuthSchema`（strict，ext.md §3.12.1）：`authorization_code` 给出配置；`client_credentials` 是不用 OAuth（None，记警告）。
/// 形状不对时整个服务器不合法。
fn oauth(value: &Value, warnings: &mut Vec<String>) -> Result<Option<McpOAuth>, String> {
    let oauth = value.as_object().ok_or("oauth: expected an object")?;
    let allowed: &[&str] = match oauth.get("type").and_then(Value::as_str) {
        Some(AUTHORIZATION_CODE) => &[
            "type",
            "clientId",
            "clientSecret",
            "clientName",
            "redirectPath",
            "scope",
        ],
        Some(CLIENT_CREDENTIALS) => &["type", "clientId", "clientSecret", "clientName", "scope"],
        _ => {
            return Err("oauth.type: expected authorization_code or client_credentials".to_owned());
        }
    };
    if let Some(unknown) = oauth.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("oauth.{unknown}: not allowed"));
    }
    let parsed = McpOAuth {
        client_id: oauth_text(oauth, "clientId", false)?,
        client_secret: oauth_text(oauth, "clientSecret", false)?.map(RedactedString::new),
        client_name: oauth_text(oauth, "clientName", false)?,
        scope: oauth_text(oauth, "scope", true)?,
        redirect_path: oauth_text(oauth, "redirectPath", false)?,
        // 只有 `mcpServers` 写法才有的两项（ext.md §3.12.15）：Node 的 schema 没有它们
        ..McpOAuth::default()
    };
    if oauth.get("type").and_then(Value::as_str) == Some(CLIENT_CREDENTIALS) {
        if parsed.client_id.is_none() || parsed.client_secret.is_none() {
            return Err("oauth: client_credentials needs clientId and clientSecret".to_owned());
        }
        warnings.push(CLIENT_CREDENTIALS_UNSUPPORTED.to_owned());
        return Ok(None);
    }
    Ok(Some(parsed))
}

/// 解析出的一个服务器：传输、是否启用、按服务器的选项与不影响连接的警告。
struct Parsed {
    transport: McpTransportConfig,
    enabled: bool,
    options: McpServerOptions,
    warnings: Vec<String>,
}

/// Node `mcpServerBaseSchema` 的其余字段（ext.md §3.1）：类型不对时整个服务器不合法（Node 的 strict 校验）。
fn extra_fields(
    server: &Map<String, Value>,
    stdio: bool,
) -> Result<(Option<String>, McpServerOptions, Vec<String>), String> {
    let cwd = match server.get("cwd") {
        None => None,
        Some(_) if !stdio => return Err("cwd: only stdio servers have a working directory".into()),
        Some(value) => Some(
            value
                .as_str()
                .filter(|cwd| !cwd.is_empty())
                .ok_or("cwd: expected a non-empty string")?
                .to_owned(),
        ),
    };
    let timeout_ms = match server.get("timeoutMs") {
        None => None,
        Some(value) => {
            let millis = value
                .as_f64()
                .filter(|millis| millis.is_finite() && *millis > 0.0)
                .ok_or("timeoutMs: expected a positive number")?;
            // 期限按整毫秒：小数向上取整。
            Some(millis.ceil() as u64)
        }
    };
    let always_load = match server.get("alwaysLoad") {
        None => false,
        Some(value) => value.as_bool().ok_or("alwaysLoad: expected a boolean")?,
    };
    // Node `McpServerIsolation`（工作线 F145）：别的值整个服务器不合法（Node 的 zod enum）。
    let isolation = match server.get("isolation").map(Value::as_str) {
        None | Some(Some("session")) => McpIsolation::Session,
        Some(Some("workspace")) => McpIsolation::Workspace,
        Some(_) => return Err("isolation: expected session or workspace".to_owned()),
    };
    // 以前三个值都按旧握手连接、非 legacy 时记警告；现在参与协商（ext.md §3.10.2）：钉死新纪元的服务器连接时失败，
    // 原因在连接状态里（不在启动警告里）。
    let protocol = match server.get("protocolVersion").map(Value::as_str) {
        None | Some(Some("auto" | "legacy")) => McpProtocol::Initialize,
        Some(Some(MODERN_REVISION)) => McpProtocol::Modern,
        Some(_) => {
            return Err(format!(
                "protocolVersion: expected auto, legacy or {MODERN_REVISION}"
            ));
        }
    };
    let warnings = Vec::new();
    let options = McpServerOptions {
        timeout_ms,
        always_load,
        isolation,
        protocol,
    };
    Ok((cwd, options, warnings))
}

/// 一个服务器；`Err` 是不合法。
fn server(value: &Value) -> Result<Parsed, String> {
    let server = normalize(value.as_object().ok_or("<server>: expected an object")?);
    let enabled = server.get("enabled") != Some(&Value::Bool(false));
    let stdio = server.get("type").and_then(Value::as_str) == Some("stdio");
    if stdio {
        required(&server, "command")?;
    }
    let (cwd, options, mut warnings) = extra_fields(&server, stdio)?;
    // stdio 不用 OAuth（规范只定义 HTTP 传输上的授权），`oauth` 照 Node 不看。
    let configured = match server.get("oauth").filter(|_| !stdio) {
        Some(value) => Some(oauth(value, &mut warnings)?),
        None => None,
    };
    let transport = match server.get("type").and_then(Value::as_str) {
        Some("stdio") => McpTransportConfig::Stdio {
            command: required(&server, "command")?,
            args: match server.get("args") {
                None => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<_>>()
                    .ok_or("args: expected an array of strings")?,
                Some(_) => return Err("args: expected an array of strings".to_owned()),
            },
            env: string_map(server.get("env"), "env")?,
            cwd,
        },
        Some(kind @ ("http" | "sse")) => {
            let url = required(&server, "url")?;
            let headers = string_map(server.get("headers"), "headers")?;
            let transport = if kind == "sse" {
                McpTransportConfig::sse(url, headers)
            } else {
                McpTransportConfig::http(url, headers)
            };
            match configured {
                Some(oauth) => transport.with_oauth(oauth),
                None => transport,
            }
        }
        Some(other) => return Err(format!("type: unsupported transport \"{other}\"")),
        None => return Err("type: required".to_owned()),
    };
    Ok(Parsed {
        transport,
        enabled,
        options,
        warnings,
    })
}

/// 服务器表的值里的服务器（按名字排序），返回服务器与警告（Node `config_mcp_server_invalid`）。`table` 是表在文件里的位置
/// （配置文件的 `mcp.servers`、`.agents/mcp.json` 的 `mcpServers`），只用在警告里。
pub fn parse_node_servers(
    servers: &Value,
    scope: McpScope,
    table: &str,
) -> (Vec<NodeMcpServer>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Some(entries) = servers.as_object() else {
        warnings.push(format!(
            "{table} must be a JSON object; ignoring all MCP servers."
        ));
        return (found, warnings);
    };
    let mut names: Vec<&String> = entries.keys().collect();
    names.sort();
    for name in names {
        match server(&entries[name]) {
            Ok(parsed) => {
                let prefix = |warning: String| format!("{table}.{name}: {warning}");
                warnings.extend(parsed.warnings.into_iter().map(prefix));
                found.push(NodeMcpServer {
                    server: McpServer {
                        name: name.clone(),
                        scope,
                        transport: parsed.transport,
                        options: parsed.options,
                        machine: Default::default(),
                    },
                    enabled: parsed.enabled,
                });
            }
            Err(reason) => warnings.push(format!("{table}.{name}: {reason}")),
        }
    }
    (found, warnings)
}

#[cfg(test)]
#[path = "node_config_tests.rs"]
mod tests;
