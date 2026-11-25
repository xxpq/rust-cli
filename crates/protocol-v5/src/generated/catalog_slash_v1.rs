//! 排版之前的摘要：9c1b23a281eea953
//! 生成物，不要手改：`catalog.slash@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySlashCommandsParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySlashCommandsResult {
    #[serde(default = "default_query_slash_commands_result_commands")]
    pub commands: Vec<SlashCommandEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlashCommandEntry {
    pub name: String,
    #[serde(default = "default_slash_command_entry_description")]
    pub description: String,
    #[serde(
        rename = "argumentHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub argument_hint: Option<String>,
    #[serde(default = "fallback_slash_command_entry_kind")]
    pub kind: SlashCommandEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SlashCommandEntryKind {
    Skill,
    Command,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SlashCommandEntryKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Command;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "skill" => Self::Skill,
            "command" => Self::Command,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Command,
            known => known.clone(),
        }
    }
}

impl Serialize for SlashCommandEntryKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SlashCommandEntryKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

fn default_slash_command_entry_description() -> String {
    String::new()
}

fn fallback_slash_command_entry_kind() -> SlashCommandEntryKind {
    SlashCommandEntryKind::Command
}

fn default_query_slash_commands_result_commands() -> Vec<SlashCommandEntry> {
    Vec::new()
}
