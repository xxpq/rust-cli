//! 交互模式的授权伪工具（docs/specs/rust-cli/ext.md §3.12.16，D56）：要授权的服务器在非 print 会话（TUI、App）里有
//! `mcp__<名字>__authenticate` 与 `mcp__<名字>__complete_authentication` 两个工具。它们像 MCP 工具一样延迟加载，不问权限；连上之后随
//! 工具快照换成真实工具。授权经进程级的登记（[`super::sign_ins`]）：authenticate 开始一次（180 s 内接着用自己开始的那一次）、给出地址，
//! 授权在后台等回调、完成后重连；complete_authentication 把用户贴回来的回调地址交给进行中的那一次，等它结束。

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use rust_cli_kernel_api::classifier::ClassifierInput;
use rust_cli_kernel_api::manage::McpSignInOutcome;
use rust_cli_kernel_api::mcp::{McpToolInfo, McpTransportConfig};
use rust_cli_kernel_api::mcp_auth::McpAuthTarget;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, failed,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::super::config::tool_name;
use super::sign_ins::{Paste, settled};
use super::{Inner, McpManager, McpServer, McpStatus};

/// 两个工具的原名（不带服务器前缀）。
const AUTHENTICATE: &str = "authenticate";
const COMPLETE: &str = "complete_authentication";
/// 结果的字符上限。
const MAX_RESULT_CHARS: usize = 10_000;
/// 服务器名在文字里最多这么多个字符。
const SHOWN_NAME_CHARS: usize = 64;
/// 描述里服务器 origin 最多这么多个字符。
const ORIGIN_CHARS: usize = 256;
/// 文字里换成空格的字符：`'` 与反引号，几种尖括号、引号与 `;`。
const SCRUBBED: &[char] = &[
    '\'', '`', '<', '>', '"', ';', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201C}', '\u{201D}',
    '\u{201E}', '\u{00AB}', '\u{00BB}', '\u{2039}', '\u{203A}', '\u{2329}', '\u{232A}', '\u{27E8}',
    '\u{27E9}', '\u{27EA}', '\u{27EB}', '\u{3008}', '\u{3009}', '\u{300A}', '\u{300B}',
];
/// 结果里说明工具什么时候能用的两句：授权还在进行时、已经连上时。
const AVAILABLE_LATER: &str = "available automatically";
const AVAILABLE_NOW: &str = "The server's tools should now be available.";

/// 文字里的服务器名：几种引号、尖括号与 `;` 换成空格，空白合并，去掉首尾空白，最多 64 个字符。
fn shown(name: &str) -> String {
    clipped(name, SHOWN_NAME_CHARS)
}

fn clipped(text: &str, limit: usize) -> String {
    let scrubbed: String = text
        .chars()
        .map(|c| if SCRUBBED.contains(&c) { ' ' } else { c })
        .collect();
    let joined = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(limit).collect()
}

/// 配置的 URL 的 origin（`scheme://host[:port]`）；不是 `scheme://…` 的形状时没有。
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|host| !host.is_empty())?;
    Some(format!("{scheme}://{host}"))
}

/// 这一个服务器的两个伪工具（要授权、不是 print 会话、能授权时才有，ext.md §3.12.16）。
pub(crate) fn stubs(config: &McpServer, manager: &Arc<Inner>) -> Vec<Arc<dyn Tool>> {
    let Some(target) = config.transport.auth_target(&config.name) else {
        return Vec::new();
    };
    let shown = shown(&config.name);
    let authenticate = tool_name(&config.name, AUTHENTICATE);
    let complete = tool_name(&config.name, COMPLETE);
    let transport = match &config.transport {
        McpTransportConfig::Http { .. } => "http",
        McpTransportConfig::Sse { .. } => "sse",
        McpTransportConfig::Stdio { .. } => "stdio",
    };
    let place = origin(&target.url)
        .map(|origin| format!("{transport} at {}", clipped(&origin, ORIGIN_CHARS)))
        .unwrap_or_else(|| transport.to_owned());
    let authenticate_description = format!(
        "The \"{shown}\" MCP server ({place}) is installed but requires authentication. Call this tool to start the OAuth flow \u{2014} you'll receive an authorization URL to share with the user. Once the user completes authorization in their browser, the server's real tools will become {AVAILABLE_LATER}."
    );
    let complete_description = format!(
        "Complete an in-progress OAuth flow for the \"{shown}\" MCP server by submitting the callback URL. Call `{authenticate}` first to start the flow and get the authorization URL. After the user authorizes in their browser, the browser is redirected to a `http://localhost:<port>/callback?code=...&state=...` URL \u{2014} on remote sessions that page fails to load, but the URL in the address bar is still valid. Pass that full URL here as `callback_url`."
    );
    let tool = |kind: Kind, name: String, description: String| -> Arc<dyn Tool> {
        Arc::new(AuthTool {
            kind,
            info: McpToolInfo {
                server: config.name.clone(),
                tool: match kind {
                    Kind::Authenticate => AUTHENTICATE,
                    Kind::Complete => COMPLETE,
                }
                .to_owned(),
                always_load: false,
            },
            name,
            description,
            authenticate: tool_name(&config.name, AUTHENTICATE),
            complete: tool_name(&config.name, COMPLETE),
            shown: shown.clone(),
            target: target.clone(),
            manager: Arc::downgrade(manager),
        })
    };
    vec![
        tool(Kind::Authenticate, authenticate, authenticate_description),
        tool(Kind::Complete, complete, complete_description),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Authenticate,
    Complete,
}

struct AuthTool {
    kind: Kind,
    name: String,
    description: String,
    info: McpToolInfo,
    authenticate: String,
    complete: String,
    shown: String,
    target: McpAuthTarget,
    manager: Weak<Inner>,
}

fn text(text: String) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error: false,
        display: Value::Null,
    }
}

impl AuthTool {
    /// authenticate：开始或接着用一次授权，给出地址。
    async fn authenticate(&self, inner: Arc<Inner>) -> String {
        let shown = &self.shown;
        let Some(sign_ins) = inner.sign_ins.get().cloned() else {
            return format!(
                "Failed to start OAuth flow for {shown}: {}. Ask the user to run /mcp and authenticate manually.",
                super::sign_ins::NO_AUTHORIZER
            );
        };
        let scope = match McpManager(inner.clone()).status_of(&self.info.server) {
            Some(McpStatus::NeedsAuth { scope }) => scope,
            _ => None,
        };
        let started = match sign_ins
            .begin(self.target.clone(), scope, true, &inner.spawner)
            .await
        {
            Ok(started) => started,
            Err(error) => {
                return format!(
                    "Failed to start OAuth flow for {shown}: {}. Ask the user to run /mcp and authenticate manually.",
                    clipped(&error, usize::MAX)
                );
            }
        };
        let reused = if started.reused {
            format!(
                "A sign-in for {shown} that started earlier is still in progress, so the link below is that sign-in's link, not a new one. If the user says the sign-in page showed an error before they could approve access, ask them to run /mcp and sign in there instead.\n\n"
            )
        } else {
            String::new()
        };
        let complete = &self.complete;
        let tail = if inner.settings().remote_shell {
            let redirect = redirect_uri(&started.url);
            format!(
                "\n\nThis session is remote, so after authorizing the browser will try to load `{redirect}?code=...` and show a connection error \u{2014} that's expected. Ask the user to copy the full URL from the browser's address bar and paste it into chat, then call `{complete}` with that URL as `callback_url`."
            )
        } else {
            format!(
                "\n\nIf the browser shows a connection error on the redirect page, ask the user to paste the full URL from the address bar and call `{complete}` with it."
            )
        };
        format!(
            "{reused}Ask the user to open this URL in their browser to authorize the {shown} MCP server:\n\n{}\n\nOnce they complete the flow, the server's tools will become {AVAILABLE_LATER}.{tail}",
            started.url
        )
    }

    /// complete_authentication：把贴回来的回调地址交给进行中的那一次，等它结束。
    async fn complete(&self, inner: Arc<Inner>, callback_url: &str) -> String {
        let (shown, authenticate) = (&self.shown, &self.authenticate);
        let no_flow = || {
            format!(
                "No OAuth flow is in progress for {shown}. Call `{authenticate}` first, then retry with the callback URL."
            )
        };
        let Some(sign_ins) = inner.sign_ins.get().cloned() else {
            return no_flow();
        };
        if !has_code_or_error(callback_url) {
            return "Invalid callback URL: missing authorization code. Ask the user to paste the full redirect URL from their browser's address bar, including the `?code=...&state=...` query string.".to_owned();
        }
        let outcome = match sign_ins.paste(&self.target, callback_url).await {
            Paste::NoFlow => return no_flow(),
            Paste::Refused => {
                return format!(
                    "That callback URL belongs to a different sign-in attempt for {shown} (its state does not match the flow in progress), or carries no authorization code. The current flow is still waiting: ask the user for the URL from the page this sign-in opened, then retry."
                );
            }
            Paste::Accepted(outcome) => outcome,
        };
        match settled(outcome).await {
            McpSignInOutcome::SignedIn => {
                format!("Authentication complete for {shown}. {AVAILABLE_NOW}")
            }
            McpSignInOutcome::Replaced | McpSignInOutcome::Ended => format!(
                "The OAuth flow for {shown} was cancelled (a newer attempt may have superseded it). Call `{authenticate}` again to restart."
            ),
            McpSignInOutcome::Failed(reason) => format!(
                "Authentication failed for {shown}: {}",
                clipped(&reason, usize::MAX)
            ),
        }
    }
}

/// 授权地址里的 redirect_uri；取不到时用占位的写法。
fn redirect_uri(url: &str) -> String {
    query_param(url, "redirect_uri")
        .unwrap_or_else(|| "http://localhost:<port>/callback".to_owned())
}

/// 回调地址的查询串里有没有 `code` 或 `error`。
fn has_code_or_error(url: &str) -> bool {
    query_param(url, "code").is_some() || query_param(url, "error").is_some()
}

/// 地址查询串里的一个参数（键不解码，值按百分号解码）。
fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| {
            percent_encoding::percent_decode_str(&value.replace('+', " "))
                .decode_utf8_lossy()
                .into_owned()
        })
    })
}

#[async_trait]
impl Tool for AuthTool {
    fn classifier_input(&self, _input: &Value) -> ClassifierInput {
        // 分类器只看服务器名
        ClassifierInput::text(self.info.server.clone())
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn input_schema(&self) -> Value {
        match self.kind {
            Kind::Authenticate => json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
            Kind::Complete => json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {"callback_url": {
                    "description": "The full callback URL from the browser address bar after authorizing, e.g. http://localhost:<port>/callback?code=...&state=...",
                    "type": "string",
                }},
                "required": ["callback_url"],
                "additionalProperties": false,
            }),
        }
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.description.clone()
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    /// 同 MCP 工具：除 `alwaysLoad` 外延迟（这两个没有 `alwaysLoad`）。
    fn mcp_info(&self) -> Option<&McpToolInfo> {
        Some(&self.info)
    }

    /// 不问权限（总是允许）。
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let inner = self
            .manager
            .upgrade()
            .ok_or_else(|| failed("MCP is shut down".to_owned()))?;
        Ok(text(match self.kind {
            Kind::Authenticate => self.authenticate(inner).await,
            Kind::Complete => {
                let url = input
                    .get("callback_url")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.complete(inner, url).await
            }
        }))
    }
}

#[cfg(test)]
#[path = "auth_tool_tests.rs"]
mod tests;
