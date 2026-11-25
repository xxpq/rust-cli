//! 外部账号层（docs/zcode-protocol-v5/22-account-overlay.md §3.3、§3.5；protocol-v5.md §36）：Host 推的进程级命令 `account.overlay.set`。
//! 账号 provider 的权益、当前连接与模型白名单的事实源在 Host；actor 校验之后同步写进进程级的格子（kernel-api 的 `ExternalAccounts`），
//! 平台层下一次取目录或端点时读到。写格子不等 IO、不等内核（§4.1「actor 不等」），所以 ACK 当场排出；没有 topic 的效果。
//!
//! ```text
//! Host ── command account.overlay.set ──► actor：严格校验 → 生成的类型 → AccountLayer → 格子.set（内容同 → noop）──► ACK
//!                                                                              │ 平台层读
//! ```
//!
//! 每一份带 Host 算它时依据的内置配置摘要（22 §15）：原样进格子。actor 不读文件、不比；平台层读到的内置配置对不上就不用这份。

use std::sync::OnceLock;

use rust_cli_kernel_api::external_accounts::{AccountGrant, AccountGrants, AccountLayer};
use rust_cli_protocol_core::schema::Node;

use crate::caps::Negotiated;
use crate::commands::{command_schema, invalid_payload, rejected};
use crate::consts::ACCOUNT_OVERLAY_CAPABILITY;
use crate::generated::account_overlay_v1::CommandAccountOverlaySet;
use crate::generated::contract::codes;
use crate::generated::core_v1::{CommandEnvelope, CommandResult, CommandResultAccepted};
use crate::server::Actor;

/// `account.overlay.set` 的命令类型（account.overlay@1）。
pub(crate) const ACCOUNT_OVERLAY_SET: &str = "account.overlay.set";
/// 给 rust 的 account.overlay@1 schema（生成物）里参数的 schema 名。
const ACCOUNT_OVERLAY_SET_SCHEMA: &str = "Command.account.overlay.set";
const ACCOUNT_OVERLAY_SCHEMA: &str = include_str!("../schema/account.overlay@1.json");

fn overlay_schema() -> Option<&'static Node> {
    static NODE: OnceLock<Option<Node>> = OnceLock::new();
    command_schema(&NODE, ACCOUNT_OVERLAY_SCHEMA, ACCOUNT_OVERLAY_SET_SCHEMA)
}

impl Actor {
    /// 握手协商的结果里没有 `account.overlay`：这个 Host 不推账号层（例如只做协议的对端），格子标为「不会来」，等第一份的组装不再等；
    /// 账号 provider 一律不授权。有这项能力的 Host 在握手 ready 时推第一份（22 §3.4 触发 1）。
    pub(crate) fn note_account_overlay(&self, negotiated: &Negotiated) {
        if !negotiated.contains_key(ACCOUNT_OVERLAY_CAPABILITY) {
            self.accounts.decline();
        }
    }

    /// `account.overlay.set`：进程级（不看 target 的工作区与会话）。参数严格：先按给 rust 的 schema 校验（带字段路径），再读成
    /// 严格类型。设值语义：与格子里现有的一份相同 → `noop`。
    pub(crate) fn set_account_overlay(&mut self, command: &CommandEnvelope) -> CommandResult {
        if let Some(schema) = overlay_schema()
            && let Err(error) = schema.validate(&command.payload, "")
        {
            return invalid_payload(&error);
        }
        let overlay =
            match serde_json::from_value::<CommandAccountOverlaySet>(command.payload.clone()) {
                Ok(overlay) => overlay,
                Err(error) => {
                    return rejected(
                        codes::PRECONDITION_INVALID_PAYLOAD,
                        Some(String::new()),
                        Some(error.to_string()),
                    );
                }
            };
        let grants: AccountGrants = overlay
            .providers
            .into_iter()
            .map(|(provider, grant)| {
                (
                    provider,
                    AccountGrant {
                        entitled: grant.entitled,
                        current: grant.current,
                        models: grant.models,
                    },
                )
            })
            .collect();
        let count = grants.len();
        let changed = self.accounts.set(AccountLayer {
            builtin_digest: overlay.builtin_config_digest,
            grants,
        });
        // 与账号变化同量级（登录、退出、换套餐），不是消息流
        tracing::info!(
            event = "account.overlay.set",
            providers = count,
            changed,
            "the host sent the account layer"
        );
        CommandResult::Accepted(CommandResultAccepted {
            effects: Vec::new(),
            noop: !changed,
            result: None,
        })
    }
}

#[cfg(test)]
#[path = "account_overlay_tests.rs"]
mod tests;
