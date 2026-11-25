//! 排版之前的摘要：0e541af9cc2e20a7
//! 生成物，不要手改：`session.goal@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGoalPause {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGoalSend {
    pub text: String,
    #[serde(
        rename = "displayText",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<CommandGoalSendDisposition>,
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<CommandGoalSendModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(
        rename = "planEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_enabled: Option<bool>,
    #[serde(
        rename = "expectedHeldQueueItemIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_held_queue_item_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandGoalSendDisposition {
    #[serde(rename = "startNow")]
    StartNow,
    #[serde(rename = "enqueue")]
    Enqueue,
    #[serde(rename = "guide")]
    Guide,
    #[serde(rename = "keepQueue")]
    KeepQueue,
    #[serde(rename = "discardQueue")]
    DiscardQueue,
}

impl CommandGoalSendDisposition {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StartNow => "startNow",
            Self::Enqueue => "enqueue",
            Self::Guide => "guide",
            Self::KeepQueue => "keepQueue",
            Self::DiscardQueue => "discardQueue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGoalSendModelSelection {
    #[serde(rename = "providerId")]
    pub provider_id: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CommandGoalSendModelSelectionOptions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGoalSendModelSelectionOptions {
    #[serde(
        rename = "reasoningLevel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultGoalSend {
    #[serde(default = "fallback_command_result_goal_send_delivery")]
    pub delivery: CommandResultGoalSendDelivery,
    #[serde(rename = "inputId")]
    pub input_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandResultGoalSendDelivery {
    StartNow,
    Queue,
    Guide,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl CommandResultGoalSendDelivery {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Queue;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::StartNow => "startNow",
            Self::Queue => "queue",
            Self::Guide => "guide",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "startNow" => Self::StartNow,
            "queue" => Self::Queue,
            "guide" => Self::Guide,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Queue,
            known => known.clone(),
        }
    }
}

impl Serialize for CommandResultGoalSendDelivery {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CommandResultGoalSendDelivery {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

pub type CommandGoalResume = CommandGoalPause;

fn fallback_command_result_goal_send_delivery() -> CommandResultGoalSendDelivery {
    CommandResultGoalSendDelivery::Queue
}
