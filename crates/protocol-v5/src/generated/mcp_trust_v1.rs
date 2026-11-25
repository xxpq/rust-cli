//! 排版之前的摘要：0786e8d40d879572
//! 生成物，不要手改：`mcp.trust@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandMcpTrustSet {
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(rename = "declarationDigest")]
    pub declaration_digest: String,
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultMcpTrustSet {}
