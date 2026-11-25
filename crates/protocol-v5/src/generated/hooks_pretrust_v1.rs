//! 排版之前的摘要：858af9d7a1829a11
//! 生成物，不要手改：`hooks.pretrust@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandHooksTrustGrant {
    #[serde(rename = "bundleDigest")]
    pub bundle_digest: String,
    #[serde(rename = "hookDeclarationDigest")]
    pub hook_declaration_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultHooksTrustGrant {
    pub accepted: bool,
    #[serde(
        rename = "reasonCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reason_code: Option<CommandResultHooksTrustGrantReasonCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandResultHooksTrustGrantReasonCode {
    WorkspaceHooksBlockedByPolicy,
    WorkspaceHooksBundleChanged,
    WorkspaceHooksSnapshotMismatch,
    WorkspaceHooksPolicyRequiresPretrust,
    WorkspaceHooksTrustStoreCorrupt,
    WorkspaceHooksConfigUnreadable,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl CommandResultHooksTrustGrantReasonCode {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::WorkspaceHooksConfigUnreadable;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::WorkspaceHooksBlockedByPolicy => "workspace_hooks_blocked_by_policy",
            Self::WorkspaceHooksBundleChanged => "workspace_hooks_bundle_changed",
            Self::WorkspaceHooksSnapshotMismatch => "workspace_hooks_snapshot_mismatch",
            Self::WorkspaceHooksPolicyRequiresPretrust => {
                "workspace_hooks_policy_requires_pretrust"
            }
            Self::WorkspaceHooksTrustStoreCorrupt => "workspace_hooks_trust_store_corrupt",
            Self::WorkspaceHooksConfigUnreadable => "workspace_hooks_config_unreadable",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "workspace_hooks_blocked_by_policy" => Self::WorkspaceHooksBlockedByPolicy,
            "workspace_hooks_bundle_changed" => Self::WorkspaceHooksBundleChanged,
            "workspace_hooks_snapshot_mismatch" => Self::WorkspaceHooksSnapshotMismatch,
            "workspace_hooks_policy_requires_pretrust" => {
                Self::WorkspaceHooksPolicyRequiresPretrust
            }
            "workspace_hooks_trust_store_corrupt" => Self::WorkspaceHooksTrustStoreCorrupt,
            "workspace_hooks_config_unreadable" => Self::WorkspaceHooksConfigUnreadable,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::WorkspaceHooksConfigUnreadable,
            known => known.clone(),
        }
    }
}

impl Serialize for CommandResultHooksTrustGrantReasonCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CommandResultHooksTrustGrantReasonCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}
