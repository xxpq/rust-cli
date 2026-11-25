//! 排版之前的摘要：21ffe7b695ef9e21
//! 生成物，不要手改：`account.overlay@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAccountOverlaySet {
    #[serde(rename = "builtinConfigDigest")]
    pub builtin_config_digest: String,
    pub providers: BTreeMap<String, CommandAccountOverlaySetProvidersValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAccountOverlaySetProvidersValue {
    pub entitled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultAccountOverlaySet {}
