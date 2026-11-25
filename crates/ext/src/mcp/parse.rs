//! MCP 服务器配置的解析与 `${VAR}` 展开（docs/specs/rust-cli/ext.md §3.1）：配置的 schema 与字符串里的变量展开。
//! `expandEnvVarsInString`）。读文件是入口层的事：它把读到的 JSON 与环境变量交给这里。

use std::collections::BTreeMap;

use rust_cli_kernel_api::mcp::{McpOAuth, McpTransportConfig};
use serde_json::Value;

use super::config::{McpScope, McpServer};

/// `${VAR}` 与 `${VAR:-默认}` 展开：规则只有一份，在 kernel-api（远端的执行器也用它，remote-exec.md §22.7）。
pub use rust_cli_kernel_api::mcp_vars::expand_vars;

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// 一个服务器的配置；不认识的形状返回原因。
fn transport(
    config: &Value,
    expand: &mut dyn FnMut(&str) -> String,
) -> Result<McpTransportConfig, String> {
    let kind = config
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("stdio");
    let url = || {
        config
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing url".to_owned())
    };
    match kind {
        "stdio" => {
            let command = config
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing command".to_owned())?;
            Ok(McpTransportConfig::Stdio {
                command: expand(command),
                args: strings(config.get("args"))
                    .iter()
                    .map(|arg| expand(arg))
                    .collect(),
                env: string_map(config.get("env"))
                    .into_iter()
                    .map(|(key, value)| (key, expand(&value)))
                    .collect(),
                cwd: None,
            })
        }
        "sse" | "http" => {
            let url = expand(url()?);
            let headers = string_map(config.get("headers"))
                .into_iter()
                .map(|(key, value)| (key, expand(&value)))
                .collect();
            let transport = if kind == "sse" {
                McpTransportConfig::sse(url, headers)
            } else {
                McpTransportConfig::http(url, headers)
            };
            // `oauth`（ext.md §3.12.1、§3.12.15）：`clientId`、`callbackPort`、`authServerMetadataUrl`、`scopes`；`xaa` 不认。
            Ok(match config.get("oauth") {
                Some(oauth) => transport.with_oauth(Some(oauth_config(oauth)?)),
                None => transport,
            })
        }
        other => Err(format!("unsupported transport type \"{other}\"")),
    }
}

/// 元数据地址不是 https 时的文字（§3.12.15）。
const METADATA_URL_NOT_HTTPS: &str = "authServerMetadataUrl must use https://";
const HTTPS: &str = "https://";

/// `mcpServers` 写法的 `oauth`（ext.md §3.12.15）：`clientId` 字符串；`callbackPort` 正整数；`authServerMetadataUrl` https 的 URL；
/// `scopes` 非空字符串。形状不对是整个服务器不合法（同别的字段）。`callbackPort` 超过 65535 也不合法。
fn oauth_config(oauth: &Value) -> Result<McpOAuth, String> {
    let text = |field: &str| -> Result<Option<String>, String> {
        match oauth.get(field) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("oauth.{field} must be a string")),
        }
    };
    let callback_port = match oauth.get("callbackPort") {
        None => None,
        Some(port) => Some(
            port.as_u64()
                .filter(|port| *port > 0)
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| "oauth.callbackPort must be a positive port number".to_owned())?,
        ),
    };
    let metadata_url = text("authServerMetadataUrl")?;
    // URL 的其余形状在取元数据时才查（platform 解析它，ext.md §3.12.15）
    if metadata_url
        .as_deref()
        .is_some_and(|url| !url.starts_with(HTTPS) || url.len() == HTTPS.len())
    {
        return Err(METADATA_URL_NOT_HTTPS.to_owned());
    }
    let scope = text("scopes")?;
    if scope.as_deref() == Some("") {
        return Err("oauth.scopes must not be empty".to_owned());
    }
    Ok(McpOAuth {
        client_id: text("clientId")?.filter(|id| !id.is_empty()),
        scope,
        callback_port,
        metadata_url,
        ..McpOAuth::default()
    })
}

/// 服务器表（`mcpServers` 的值）里的服务器，按名字排序；返回服务器与警告。
pub fn parse_servers(
    servers: &Value,
    scope: McpScope,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (Vec<McpServer>, Vec<String>) {
    parse_servers_keeping(servers, scope, lookup, &|_| false)
}

/// 同 [`parse_servers`]；`keep(名字)` 为真的服务器按原文、不展开 `${VAR}`（绑定到远端机器的，本机的值不进请求）。
pub fn parse_servers_keeping(
    servers: &Value,
    scope: McpScope,
    lookup: &dyn Fn(&str) -> Option<String>,
    keep: &dyn Fn(&str) -> bool,
) -> (Vec<McpServer>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Some(entries) = servers.as_object() else {
        if !servers.is_null() {
            warnings.push("mcpServers must be a JSON object".to_owned());
        }
        return (found, warnings);
    };
    let mut names: Vec<&String> = entries.keys().collect();
    names.sort();
    for name in names {
        let mut missing = Vec::new();
        let parsed = {
            let mut expand = |text: &str| match keep(name) {
                // 绑定到远端机器的服务器（R5，remote-exec.md §22.7）：按原文，`${VAR}` 留给那台机器的执行器展开。
                true => text.to_owned(),
                false => expand_vars(text, lookup, &mut missing),
            };
            transport(&entries[name], &mut expand)
        };
        match parsed {
            Ok(transport) => {
                if !missing.is_empty() {
                    warnings.push(format!(
                        "MCP server \"{name}\": missing environment variables: {}",
                        missing.join(", ")
                    ));
                }
                found.push(McpServer {
                    name: name.clone(),
                    scope,
                    transport,
                    options: Default::default(),
                    machine: Default::default(),
                });
            }
            Err(reason) => warnings.push(format!("MCP server \"{name}\" skipped: {reason}")),
        }
    }
    (found, warnings)
}

/// 按作用域合并（ext.md §3.1）：enterprise 存在时只用它；否则 user < project < local，dynamic 覆盖同名。
pub fn merge_scopes(layers: Vec<Vec<McpServer>>) -> Vec<McpServer> {
    let all: Vec<McpServer> = layers.into_iter().flatten().collect();
    if all
        .iter()
        .any(|server| server.scope == McpScope::Enterprise)
    {
        return all
            .into_iter()
            .filter(|server| server.scope == McpScope::Enterprise)
            .collect();
    }
    // 同名时后者覆盖前者：插件 < 用户 < 项目 < 本地 < 动态与企业。
    const PLUGIN_RANK: u8 = 0;
    const USER_RANK: u8 = 1;
    const PROJECT_RANK: u8 = 2;
    const LOCAL_RANK: u8 = 3;
    const DYNAMIC_RANK: u8 = 4;
    let rank = |scope: McpScope| match scope {
        McpScope::Plugin => PLUGIN_RANK,
        McpScope::User => USER_RANK,
        McpScope::Project => PROJECT_RANK,
        McpScope::Local => LOCAL_RANK,
        McpScope::Dynamic | McpScope::Enterprise => DYNAMIC_RANK,
    };
    let mut merged: BTreeMap<String, McpServer> = BTreeMap::new();
    for server in all {
        let keep = merged
            .get(&server.name)
            .is_none_or(|known| rank(server.scope) >= rank(known.scope));
        if keep {
            merged.insert(server.name.clone(), server);
        }
    }
    merged.into_values().collect()
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
