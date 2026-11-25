//! 远程 MCP 服务器的授权（protocol-v5.md §74.2；docs/zcode-protocol-v5/18-mcp-trust.md §16.3）：命令 `mcp.oauth.signIn`、`mcp.oauth.signOut`，
//! 查询 `mcp.oauth.signInOutcome`、`mcp.oauth.servers`。只在入口装了授权端口时认它们（`Settings.mcp_oauth`，同插件管理按端口声明）。
//!
//! ```text
//! actor: 校验目标与参数（严格）─► 任务：Workspaces::open(工作区)（组装中等它，没有就组装）─► 内核的 kernel-api 方法 ─► Event::McpOAuthDone
//! actor: 直接写出回复（没有 topic 的效果，不排出站队列）
//! ```
//!
//! actor 不等：开始授权要发现与注册（网络），结局要等用户在浏览器里点。这些任务不进 `shutdown` 的在途计数：进行中的授权只开了监听，
//! 不写共享记录，进程结束时监听随之关掉（§74.2）。

use std::sync::{Arc, OnceLock};

use rust_cli_kernel_api::manage::McpSignInOutcome;
use rust_cli_protocol_core::schema::Node;
use serde_json::Value;

use crate::commands::{command_schema, field_path, invalid_payload, rejected, workspace_key};
use crate::errors::{Fault, protocol_error, to_rpc};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandResultAccepted, QueryEnvelope, QueryResult,
    QueryResultOk,
};
use crate::generated::mcp_oauth_v1::{
    CommandMcpOauthSignIn, CommandResultMcpOauthSignIn, QueryMcpOauthServersResult,
    QueryMcpOauthServersResultServersItem, QueryMcpOauthSignInOutcomeParams,
    QueryMcpOauthSignInOutcomeResult, QueryMcpOauthSignInOutcomeResultOutcome,
};
use crate::server::{Actor, Event, Workspaces};
use crate::topics;

pub(crate) const SIGN_IN: &str = "mcp.oauth.signIn";
pub(crate) const SIGN_OUT: &str = "mcp.oauth.signOut";
pub(crate) const SIGN_IN_OUTCOME: &str = "mcp.oauth.signInOutcome";
pub(crate) const SERVERS: &str = "mcp.oauth.servers";
const SCHEMA: &str = include_str!("../schema/mcp.oauth@1.json");
/// 进程级部件没建好。
const NOT_READY: &str = "the process is not ready yet";

pub(crate) fn is_command(kind: &str) -> bool {
    kind == SIGN_IN || kind == SIGN_OUT
}

pub(crate) fn is_query(kind: &str) -> bool {
    kind == SIGN_IN_OUTCOME || kind == SERVERS
}

fn schema(name: &'static str) -> Option<&'static Node> {
    static SIGN_IN_NODE: OnceLock<Option<Node>> = OnceLock::new();
    static SIGN_OUT_NODE: OnceLock<Option<Node>> = OnceLock::new();
    static OUTCOME_NODE: OnceLock<Option<Node>> = OnceLock::new();
    static SERVERS_NODE: OnceLock<Option<Node>> = OnceLock::new();
    let cell = match name {
        SIGN_IN => &SIGN_IN_NODE,
        SIGN_OUT => &SIGN_OUT_NODE,
        SIGN_IN_OUTCOME => &OUTCOME_NODE,
        _ => &SERVERS_NODE,
    };
    let schema_name = match name {
        SIGN_IN => "Command.mcp.oauth.signIn",
        SIGN_OUT => "Command.mcp.oauth.signOut",
        SIGN_IN_OUTCOME => "Query.mcp.oauth.signInOutcome.params",
        _ => "Query.mcp.oauth.servers.params",
    };
    command_schema(cell, SCHEMA, schema_name)
}

fn accepted(result: Option<Value>, noop: bool) -> CommandResult {
    CommandResult::Accepted(CommandResultAccepted {
        effects: Vec::new(),
        noop,
        result,
    })
}

fn fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

fn query_ok(data: Value) -> Value {
    serde_json::to_value(QueryResult::Ok(QueryResultOk { data, cursor: None }))
        .unwrap_or(Value::Null)
}

/// 这个工作区的内核里有没有这个用 OAuth 的服务器（区分「没有 / 不用 OAuth」与「授权做不成」两个拒绝码，18 §16.3）。
async fn knows(kernel: &Arc<dyn rust_cli_kernel_api::session_api::Kernel>, name: &str) -> bool {
    kernel
        .mcp_oauth_servers()
        .await
        .iter()
        .any(|server| server.name == name)
}

fn unknown_server(name: &str) -> String {
    format!("this workspace has no MCP server \"{name}\" that signs in with OAuth")
}

/// 命令在任务里做的那一段：组装（或等）工作区内核，调 kernel-api。
async fn run_command(
    workspaces: Arc<dyn Workspaces>,
    key: String,
    kind: String,
    server: String,
) -> CommandResult {
    let kernel = match workspaces.open(&key).await {
        Ok(opened) => opened.kernel,
        Err(message) => {
            return rejected(codes::PRECONDITION_STORE_UNAVAILABLE, None, Some(message));
        }
    };
    if !knows(&kernel, &server).await {
        return rejected(
            codes::PRECONDITION_MCP_OAUTH_UNAVAILABLE,
            None,
            Some(unknown_server(&server)),
        );
    }
    let failed =
        |message: String| rejected(codes::PRECONDITION_MCP_OAUTH_FAILED, None, Some(message));
    if kind == SIGN_IN {
        // 授权由进程级的登记持有（ext.md §3.12.14）：`done` 不要，结局经 signInOutcome 取
        return match kernel.mcp_sign_in(server).await {
            Ok(started) => accepted(
                serde_json::to_value(CommandResultMcpOauthSignIn {
                    authorization_url: started.url,
                })
                .ok(),
                false,
            ),
            Err(error) => failed(error.to_string()),
        };
    }
    let signed_in = kernel
        .mcp_oauth_servers()
        .await
        .into_iter()
        .any(|known| known.name == server && known.signed_in);
    match kernel.mcp_sign_out(server).await {
        // 没存过令牌：要的已经是现状
        Ok(_) => accepted(None, !signed_in),
        Err(error) => failed(error.to_string()),
    }
}

/// 查询在任务里做的那一段。
async fn run_query(
    workspaces: Arc<dyn Workspaces>,
    key: String,
    kind: String,
    params: Value,
) -> Result<Value, Fault> {
    let kernel = workspaces
        .open(&key)
        .await
        .map_err(|message| fault(codes::PRECONDITION_STORE_UNAVAILABLE, message))?
        .kernel;
    if kind == SERVERS {
        let servers = kernel
            .mcp_oauth_servers()
            .await
            .into_iter()
            .map(|server| QueryMcpOauthServersResultServersItem {
                name: server.name,
                signed_in: server.signed_in,
                needs_auth: server.needs_auth,
                pending_authorization_url: server.pending_url,
            })
            .collect();
        let data =
            serde_json::to_value(QueryMcpOauthServersResult { servers }).unwrap_or(Value::Null);
        return Ok(query_ok(data));
    }
    let params: QueryMcpOauthSignInOutcomeParams = serde_json::from_value(params)
        .map_err(|error| fault(codes::PRECONDITION_INVALID_PAYLOAD, error.to_string()))?;
    if !knows(&kernel, &params.server_name).await {
        return Err(fault(
            codes::PRECONDITION_MCP_OAUTH_UNAVAILABLE,
            unknown_server(&params.server_name),
        ));
    }
    let outcome = kernel
        .mcp_sign_in_outcome(params.server_name, params.authorization_url)
        .await
        .map_err(|error| fault(codes::PRECONDITION_MCP_OAUTH_UNAVAILABLE, error.to_string()))?;
    let (outcome, message) = match outcome {
        McpSignInOutcome::SignedIn => (QueryMcpOauthSignInOutcomeResultOutcome::SignedIn, None),
        McpSignInOutcome::Failed(reason) => (
            QueryMcpOauthSignInOutcomeResultOutcome::Failed,
            Some(reason),
        ),
        McpSignInOutcome::Replaced => (QueryMcpOauthSignInOutcomeResultOutcome::Replaced, None),
        McpSignInOutcome::Ended => (QueryMcpOauthSignInOutcomeResultOutcome::Ended, None),
    };
    let data = serde_json::to_value(QueryMcpOauthSignInOutcomeResult { outcome, message })
        .unwrap_or(Value::Null);
    Ok(query_ok(data))
}

impl Actor {
    /// `mcp.oauth.signIn` / `signOut`（§74.2）：校验，通过的交给任务；回复等 [`Event::McpOAuthDone`]。没通过的当场回。
    pub(crate) fn mcp_oauth_command(&mut self, id: &Value, command: &CommandEnvelope) {
        let checked = workspace_key(command).and_then(|key| {
            if let Some(schema) = schema(if command.r#type == SIGN_IN {
                SIGN_IN
            } else {
                SIGN_OUT
            }) && let Err(error) = schema.validate(&command.payload, "")
            {
                return Err(Box::new(invalid_payload(&error)));
            }
            let payload = serde_json::from_value::<CommandMcpOauthSignIn>(command.payload.clone())
                .map_err(|error| {
                    Box::new(rejected(
                        codes::PRECONDITION_INVALID_PAYLOAD,
                        Some(String::new()),
                        Some(error.to_string()),
                    ))
                })?;
            let (workspaces, _) = self.sources().ok_or_else(|| {
                Box::new(rejected(
                    codes::PRECONDITION_STORE_UNAVAILABLE,
                    None,
                    Some(NOT_READY.to_owned()),
                ))
            })?;
            Ok((key, payload.server_name, workspaces))
        });
        let (key, server, workspaces) = match checked {
            Ok(checked) => checked,
            Err(result) => {
                let value = serde_json::to_value(*result).unwrap_or(Value::Null);
                self.outbox.reply(id.clone(), Ok(value));
                return;
            }
        };
        let (events, reply, kind) = (self.events.clone(), id.clone(), command.r#type.clone());
        drop(self.spawner.spawn("protocol-v5-mcp-oauth", async move {
            let result = run_command(workspaces, key, kind, server).await;
            let result = Ok(serde_json::to_value(result).unwrap_or(Value::Null));
            let _ = events.send(Event::McpOAuthDone { reply, result });
        }));
    }

    /// `mcp.oauth.signInOutcome` / `servers`（§74.2）：校验（目标同 `sessions.facts`、参数严格），通过的交给任务。
    pub(crate) fn mcp_oauth_query(
        &mut self,
        id: &Value,
        query: QueryEnvelope,
    ) -> Result<(), Fault> {
        let Some(key) = query.target.workspace_key.clone() else {
            return Err(fault(
                codes::PRECONDITION_WORKSPACE_REQUIRED,
                format!("{} needs target.workspaceKey", query.r#type),
            ));
        };
        topics::local_workspace(&key)
            .map_err(|message| fault(codes::PRECONDITION_INVALID, message))?;
        let params = query
            .params
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        if let Some(schema) = schema(if query.r#type == SERVERS {
            SERVERS
        } else {
            SIGN_IN_OUTCOME
        }) && let Err(error) = schema.validate(&params, "")
        {
            let (field, message) = field_path(&error);
            return Err(fault(
                codes::PRECONDITION_INVALID_PAYLOAD,
                format!("{field}: {message}"),
            ));
        }
        let Some((workspaces, _)) = self.sources() else {
            return Err(fault(codes::PRECONDITION_STORE_UNAVAILABLE, NOT_READY));
        };
        let (events, reply, kind) = (self.events.clone(), id.clone(), query.r#type);
        drop(self.spawner.spawn("protocol-v5-mcp-oauth", async move {
            let result = run_query(workspaces, key, kind, params).await;
            let _ = events.send(Event::McpOAuthDone { reply, result });
        }));
        Ok(())
    }

    /// 任务回来了：直接写出回复（没有效果帧要排在前面）。
    pub(crate) async fn mcp_oauth_done(&mut self, reply: Value, result: Result<Value, Fault>) {
        self.peer
            .respond(&reply, &result.map_err(|error| to_rpc(&error)))
            .await;
    }
}

#[cfg(test)]
#[path = "mcp_oauth_tests.rs"]
mod tests;
