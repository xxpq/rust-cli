//! 握手（docs/zcode-protocol-v5/03-capabilities-handshake.md §3.1–§3.3；protocol-v5.md §5）：initialize → 结果或拒绝。
//! 判定顺序：结果恰好一次 → 契约指纹 → 按修订协商。带了指纹且相等 → 这条链路之后严格读（03 §3.8）。

use serde_json::Value;

use crate::caps::{self, Declaration, Missing};
use crate::consts::{
    CATALOG_PLUGINS_CAPABILITY, CORE_PATCH_CAPABILITY, INITIAL_CREDIT, MAX_MESSAGE_BYTES,
    MCP_OAUTH_CAPABILITY, SERVER_KIND,
};
use crate::errors::{Fault, protocol_error, with_details};
use crate::generated::contract::{CONTRACT_DIGEST, INITIALIZE, codes};
use crate::generated::core_v1::{
    InitializeParams, InitializeParamsPeerBuild, InitializeRejection, InitializeRejectionContract,
    InitializeRejectionUpgrade, InitializeResult, InitializeResultDelivery, InitializeResultLimits,
    InitializeResultPrincipal, InitializeResultServer,
};
use crate::server::{Actor, decode, violation};

/// 被拒（03 §3.3）：原因只在 ProtocolError.code，details 只带缺了什么。
fn rejected(code: &str, message: String, details: InitializeRejection) -> Fault {
    let details = serde_json::to_value(details).unwrap_or(Value::Null);
    Box::new(with_details(protocol_error(code, message), details))
}

impl Actor {
    pub(crate) fn initialize(&mut self, params: Value) -> Result<Value, Fault> {
        // 一律宽松读：外壳冻结，任何修订的对端都要读得懂拒绝原因（03 §3.8）
        let params: InitializeParams = decode(INITIALIZE, params)?;
        if self.negotiated.is_some() {
            // 结果恰好一次（03 §3.3）
            return Err(violation("initialize already succeeded"));
        }
        if let Some(theirs) = &params.contract
            && theirs != CONTRACT_DIGEST
        {
            // 两端不是同一份契约生成的（03 §3.7）：Host 据此走启动回退（03 §3.5 的“契约不匹配”）
            let message = format!(
                "contract digest mismatch: this side {CONTRACT_DIGEST}, the peer {theirs}; rebuild the rust CLI from the same tree"
            );
            let contract = InitializeRejectionContract {
                client: theirs.clone(),
                server: CONTRACT_DIGEST.to_owned(),
            };
            return Err(rejected(
                codes::HANDSHAKE_CONTRACT_MISMATCH,
                message,
                InitializeRejection {
                    missing: None,
                    upgrade: None,
                    contract: Some(contract),
                },
            ));
        }
        let theirs: Declaration = params.caps.clone();
        let mut mine = caps::served();
        // 插件管理只在入口交了端口时声明（§72.1）：今天的 cli 不交，真二进制不声明
        if self.plugins.admin.is_some() {
            mine.extend(caps::declared(CATALOG_PLUGINS_CAPABILITY));
        }
        // MCP 的授权只在入口装了时声明（§74）：接 App 的入口装，测试的替身不装
        if self.settings.mcp_oauth {
            mine.extend(caps::declared(MCP_OAUTH_CAPABILITY));
        }
        let negotiated = match caps::negotiate(&mine, &theirs, &params.require) {
            Ok(negotiated) => negotiated,
            Err((missing, target)) => {
                let names: Vec<&str> = missing.iter().map(|m| m.cap.as_str()).collect();
                let message = format!("missing required capabilities: {}", names.join(", "));
                let upgrade = InitializeRejectionUpgrade {
                    target,
                    message: message.clone(),
                };
                return Err(rejected(
                    codes::HANDSHAKE_MISSING_CAPABILITIES,
                    message,
                    InitializeRejection {
                        missing: Some(missing.iter().map(Missing::to_wire).collect()),
                        upgrade: Some(upgrade),
                        contract: None,
                    },
                ));
            }
        };
        // 来源授权（03 §4）不做第二步：CLI 的 client 只有所属 Host（upstream-host，03 §3.5），本端的能力都对它开放，
        // 协商结果就是授权结果
        let result = InitializeResult {
            connection_id: self.connection.clone(),
            principal: InitializeResultPrincipal::UpstreamHost,
            delivery: InitializeResultDelivery::Continuous,
            caps: negotiated.clone(),
            limits: InitializeResultLimits {
                max_message_bytes: u64::try_from(MAX_MESSAGE_BYTES).unwrap_or(u64::MAX),
                initial_credit: INITIAL_CREDIT,
            },
            server: InitializeResultServer {
                kind: SERVER_KIND.to_owned(),
                build: InitializeParamsPeerBuild {
                    version: self.settings.version.clone(),
                    commit: None,
                },
                host_id: None,
            },
            contract: CONTRACT_DIGEST.to_owned(),
        };
        // 没协商出 account.overlay 的 Host 不推账号层（22 §3.5 第 6 项）：组装的缺省模型不再等它
        self.note_account_overlay(&negotiated);
        // 协商出 core.patch 的连接收到 entity.patch，别的收到同一项当前整行的 put（01 §19.4）
        self.outbox
            .allow_patch(negotiated.contains_key(CORE_PATCH_CAPABILITY));
        self.negotiated = Some(negotiated);
        self.strict = params.contract.is_some();
        // Host 应答的服务（protocol-v5.md §30）：不带 = 什么也不应答
        self.declare_host_services(params.host_services.unwrap_or_default());
        tracing::info!(
            event = "protocol.handshake.initialized",
            connection = %self.connection,
            peer = params.peer.name,
            strict = self.strict,
            "the peer completed the handshake"
        );
        Ok(serde_json::to_value(result).unwrap_or(Value::Null))
    }
}
