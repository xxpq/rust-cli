//! 工作区级的 hooks 预信任（protocol-v5.md §59.2；docs/zcode-protocol-v5/17-host-seam.md §19.4）：命令 `hooks.trustGrant`。
//! 判定与写入在入口（cli，经 [`crate::Workspaces::pretrust_hooks`]）：它用 `zcode hooks trust grant` 的同一段代码写记录，接受了就重读这个
//! 工作区的 hooks、整份替换内核的快照（config.md §4.5）。actor 只校验命令，把写入交给本链路唯一的信任写入工作任务（[`crate::trust_writes`]，
//! 与 `mcp.trust.set` 共用），回复先不发；任务做完发 [`Event::HooksPretrusted`] 回来，actor 排 ACK。
//!
//! ```text
//! actor: command hooks.trustGrant ─ 校验（目标、参数严格）─► 工作任务的队列（按到达顺序）；回复记下，接着处理别的消息
//! 工作任务: Workspaces::pretrust_hooks（锁 → 重新发现声明 → 比 bundle 与 digest → 写 → 重读状态 → 重读 hooks、替换快照）─► Event::HooksPretrusted
//! actor: ACK accepted{result: {accepted, reasonCode?}}（否定结论是业务结果，不是 rejected）
//! ```

use std::sync::OnceLock;

use rust_cli_protocol_core::schema::Node;
use serde_json::Value;

use crate::commands::{command_schema, invalid_payload, rejected, workspace_key};
use crate::generated::contract::codes;
use crate::generated::core_v1::{CommandEnvelope, CommandResult, CommandResultAccepted};
use crate::generated::hooks_pretrust_v1::{
    CommandHooksTrustGrant, CommandResultHooksTrustGrant, CommandResultHooksTrustGrantReasonCode,
};
use crate::server::{Actor, Event};

/// `hooks.trustGrant` 的命令类型（hooks.pretrust@1）。
pub(crate) const HOOKS_TRUST_GRANT: &str = "hooks.trustGrant";
/// 给 rust 的 hooks.pretrust@1 schema（生成物）里参数的 schema 名。
const TRUST_GRANT_SCHEMA: &str = "Command.hooks.trustGrant";
const SCHEMA: &str = include_str!("../schema/hooks.pretrust@1.json");

/// `hooks.trustGrant` 的参数：这一版声明的 bundle 与要信任的那一条声明的 digest。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookPretrust {
    pub bundle: String,
    pub digest: String,
}

/// 没有接受的原因：契约的公开原因码（Node `toPublicReasonCode`：不是公开码的一律收成 `workspace_hooks_config_unreadable`）。
pub type HookPretrustRefusal = CommandResultHooksTrustGrantReasonCode;

fn grant_schema() -> Option<&'static Node> {
    static NODE: OnceLock<Option<Node>> = OnceLock::new();
    command_schema(&NODE, SCHEMA, TRUST_GRANT_SCHEMA)
}

impl Actor {
    /// `hooks.trustGrant`（§59.2）：校验，通过的交给信任写入的工作任务、返回 None（ACK 等 [`Event::HooksPretrusted`]）；没通过的返回结果。
    pub(crate) fn pretrust_hooks(
        &mut self,
        reply: &Value,
        command: &CommandEnvelope,
    ) -> Option<CommandResult> {
        let key = match workspace_key(command) {
            Ok(key) => key,
            Err(result) => return Some(*result),
        };
        if let Some(schema) = grant_schema()
            && let Err(error) = schema.validate(&command.payload, "")
        {
            return Some(invalid_payload(&error));
        }
        let payload =
            match serde_json::from_value::<CommandHooksTrustGrant>(command.payload.clone()) {
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
        let request = HookPretrust {
            bundle: payload.bundle_digest,
            digest: payload.hook_declaration_digest,
        };
        let reply = reply.clone();
        let job = Box::pin(async move {
            let result = workspaces.pretrust_hooks(&key, request).await;
            Event::HooksPretrusted { reply, result }
        });
        self.trusts.submit(job, &self.events, &self.spawner);
        None
    }

    /// 工作任务做完一条（§59.2）：重读已在入口里做完（效果在 ACK 之前），这里只排 ACK。没有会话 topic 的效果：rust 不产出 `hookAdmission`。
    pub(crate) fn hooks_pretrusted(
        &mut self,
        reply: Value,
        result: Result<(), HookPretrustRefusal>,
    ) {
        let outcome = CommandResultHooksTrustGrant {
            accepted: result.is_ok(),
            reason_code: result.err(),
        };
        let result = CommandResult::Accepted(CommandResultAccepted {
            effects: Vec::new(),
            // 没接受时什么都没写
            noop: !outcome.accepted,
            result: Some(serde_json::to_value(outcome).unwrap_or(Value::Null)),
        });
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        self.outbox.reply(reply, Ok(value));
        self.trusts.done();
        self.finish_shutdown();
    }
}

#[cfg(test)]
#[path = "hook_pretrust_tests.rs"]
mod tests;
