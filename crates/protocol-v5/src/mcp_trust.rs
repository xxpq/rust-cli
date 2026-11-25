//! 工作区 MCP 的信任（docs/zcode-protocol-v5/18-mcp-trust.md §12.5、§12.6；protocol-v5.md §16）：命令 `mcp.trust.set` 与会话 state
//! 的 `mcpAdmission`。判定与写入在入口（cli，经 [`crate::Workspaces`] 端口）：它在信任记录的锁里按磁盘上的声明重验 digest，再写记录。
//! actor 只校验命令，把写入交给本链路唯一的信任写入工作任务（[`crate::trust_writes`]，与 `hooks.trustGrant` 共用），回复先不发；
//! 工作任务做完发 [`Event::McpTrusted`] 回来，actor 再把写入之后的待信任声明投影到这个工作区的活会话，最后排 ACK（效果之后，G2）。
//!
//! ```text
//! actor: command mcp.trust.set ─ 校验（目标、参数严格）─► 工作任务的队列（按到达顺序）；回复记下，接着处理别的消息
//! 工作任务: Workspaces::trust_mcp（锁 → 重读声明 → 比 digest → 写 → 补连 → 待信任的声明）─► Event::McpTrusted
//! actor: 每个活会话 state.set{mcpAdmission} ─► ACK
//! ```

use std::sync::OnceLock;

use rust_cli_base::hash::sha256_hex;
use rust_cli_base::js::json::stringify;
use rust_cli_protocol_core::schema::Node;
use serde_json::{Value, json};

use crate::commands::{command_schema, invalid_payload, rejected, workspace_key};
use crate::generated::contract::codes;
use crate::generated::core_v1::{CommandEnvelope, CommandResult, CommandResultAccepted};
use crate::generated::mcp_trust_v1::CommandMcpTrustSet;
use crate::server::{Actor, Event};

/// `mcp.trust.set` 的命令类型（mcp.trust@1）。
pub(crate) const MCP_TRUST_SET: &str = "mcp.trust.set";
/// 给 rust 的 mcp.trust@1 schema（生成物）里参数的 schema 名。
const MCP_TRUST_SET_SCHEMA: &str = "Command.mcp.trust.set";
const MCP_TRUST_SCHEMA: &str = include_str!("../schema/mcp.trust@1.json");

/// 工作区里等用户信任的一份项目级声明：服务器名与那份声明的 digest（入口按记录算出，18 §12.5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPending {
    pub server: String,
    pub digest: String,
}

/// `mcp.trust.set` 的决定（参数原样）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpDecision {
    pub server: String,
    pub digest: String,
    pub trusted: bool,
}

/// 写入成功：有没有改记录（没改 = 结果已是现状，`noop`），与写入之后这个工作区的待信任声明（重算活会话的 `mcpAdmission`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTrusted {
    pub changed: bool,
    pub pending: Vec<McpPending>,
}

/// 写入被拒：锁内重读之后声明变了（或没有这个声明）；或声明、记录读不出，锁、写失败（`message` 是诊断原文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTrustRefusal {
    DeclarationChanged,
    Unavailable(String),
}

/// 会话 state 的 `mcpAdmission`（18 §12.5）：没有待信任的声明时为 null；`bundleDigest` 是 `[[名字, digest], …]`（按名字）的
/// sha256，只给界面判“提示变没变”。
pub(crate) fn admission(pending: &[McpPending]) -> Value {
    if pending.is_empty() {
        return Value::Null;
    }
    let mut pairs: Vec<(&str, &str)> = pending
        .iter()
        .map(|item| (item.server.as_str(), item.digest.as_str()))
        .collect();
    pairs.sort_unstable();
    let bundle = json!(
        pairs
            .iter()
            .map(|(server, digest)| json!([server, digest]))
            .collect::<Vec<_>>()
    );
    json!({"pendingCount": pending.len(), "bundleDigest": sha256_hex(stringify(&bundle).as_bytes())})
}

fn trust_schema() -> Option<&'static Node> {
    static NODE: OnceLock<Option<Node>> = OnceLock::new();
    command_schema(&NODE, MCP_TRUST_SCHEMA, MCP_TRUST_SET_SCHEMA)
}

impl Actor {
    /// `mcp.trust.set`（18 §12.5、§12.6）：校验，通过的交给工作任务、返回 None（ACK 等 [`Event::McpTrusted`]）；没通过的返回结果。
    ///
    /// 缺陷修复（主会话审查 MCP-TRUST-A）：以前在这里 `await` 入口的写入。写入要等 Node 的目录锁（最多 8 s）与文件 IO，actor 是整条链路
    /// 唯一的 owner，这段时间里全部工作区、全部会话的请求、通道事件与 `ping` 都排在后面。改成交给工作任务，actor 只发帧与排 ACK。
    pub(crate) fn trust_mcp(
        &mut self,
        reply: &Value,
        command: &CommandEnvelope,
    ) -> Option<CommandResult> {
        let key = match workspace_key(command) {
            Ok(key) => key,
            Err(result) => return Some(*result),
        };
        if let Some(schema) = trust_schema()
            && let Err(error) = schema.validate(&command.payload, "")
        {
            return Some(invalid_payload(&error));
        }
        let payload = match serde_json::from_value::<CommandMcpTrustSet>(command.payload.clone()) {
            Ok(payload) => payload,
            Err(error) => {
                return Some(rejected(
                    codes::PRECONDITION_INVALID_PAYLOAD,
                    Some(String::new()),
                    Some(error.to_string()),
                ));
            }
        };
        let Some((workspaces, _)) = self.sources() else {
            return Some(rejected(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                None,
                Some("the process is not ready yet".to_owned()),
            ));
        };
        let decision = McpDecision {
            server: payload.server_name,
            digest: payload.declaration_digest,
            trusted: payload.trusted,
        };
        let reply = reply.clone();
        let job = Box::pin(async move {
            let result = workspaces.trust_mcp(&key, decision).await;
            Event::McpTrusted { reply, key, result }
        });
        self.trusts.submit(job, &self.events, &self.spawner);
        None
    }

    /// 工作任务做完一条（18 §12.6）：写入成功时这个工作区的活会话先换上写入之后的 `mcpAdmission`，再排 ACK（效果之后，G2）。
    pub(crate) fn mcp_trusted(
        &mut self,
        reply: Value,
        key: &str,
        result: Result<McpTrusted, McpTrustRefusal>,
    ) {
        let result = match result {
            Ok(trusted) => {
                let effects = self.mcp_admission_changed(key, &admission(&trusted.pending));
                CommandResult::Accepted(CommandResultAccepted {
                    effects,
                    noop: !trusted.changed,
                    result: None,
                })
            }
            Err(McpTrustRefusal::DeclarationChanged) => {
                rejected(codes::PRECONDITION_MCP_DECLARATION_CHANGED, None, None)
            }
            Err(McpTrustRefusal::Unavailable(message)) => rejected(
                codes::PRECONDITION_MCP_TRUST_UNAVAILABLE,
                None,
                Some(message),
            ),
        };
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        self.outbox.reply(reply, Ok(value));
        self.trusts.done();
        self.finish_shutdown();
    }
}

#[cfg(test)]
#[path = "mcp_trust_tests.rs"]
mod tests;
