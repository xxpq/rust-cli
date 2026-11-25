//! 排版之前的摘要：39da97eb7180b37f
//! 生成物，不要手改：`session.report@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRowSetFeedback {
    pub feedback: CommandRowSetFeedbackFeedback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandRowSetFeedbackFeedback {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "up")]
    Up,
    #[serde(rename = "down")]
    Down,
}

impl CommandRowSetFeedbackFeedback {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}
