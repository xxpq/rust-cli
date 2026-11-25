//! 排版之前的摘要：ccf4645e626d02ee
//! 生成物，不要手改：`session.queue@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use crate::tolerant::Opaque;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueApplyConfig {
    #[serde(rename = "queueItemIds")]
    pub queue_item_ids: Vec<String>,
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<CommandQueueApplyConfigModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueApplyConfigModelSelection {
    #[serde(rename = "providerId")]
    pub provider_id: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CommandQueueApplyConfigModelSelectionOptions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueApplyConfigModelSelectionOptions {
    #[serde(
        rename = "reasoningLevel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueDelete {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueEdit {
    #[serde(rename = "newText")]
    pub new_text: String,
    #[serde(rename = "editorId", default, skip_serializing_if = "Option::is_none")]
    pub editor_id: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Opaque>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotes: Option<Vec<CommandQueueEditQuotesItem>>,
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<CommandQueueApplyConfigModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(
        rename = "planEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueEditQuotesItem {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(
        rename = "senderName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sender_name: Option<String>,
    #[serde(rename = "senderId", default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(rename = "messageId", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(rename = "sentAt", default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueLockEdit {
    #[serde(rename = "editorId")]
    pub editor_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueReorder {
    #[serde(rename = "beforeId")]
    pub before_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueSendNow {
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandQueueSetAutoDrain {
    #[serde(rename = "autoDrain")]
    pub auto_drain: bool,
}

pub type CommandQueueUnlockEdit = CommandQueueDelete;
