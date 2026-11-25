//! 排版之前的摘要：f3771080c7770e0f
//! 生成物，不要手改：`agent@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::{
    AgentCuaEndReason, AgentCuaPermissionRequestMissingItem,
    AgentCuaPermissionRequestPermissionStatusAccessibility,
    AgentCuaPermissionRequestPermissionStatusScreenRecording, AgentExitReason, AgentStatePhase,
    AgentStateProvidersItem, AgentStateStorageMigrationKind, AgentStateStoragePhase,
};

pub(super) fn fallback_agent_cua_end_reason() -> AgentCuaEndReason {
    AgentCuaEndReason::TurnFailed
}

pub(super) fn default_agent_cua_permission_request_missing()
-> Vec<AgentCuaPermissionRequestMissingItem> {
    Vec::new()
}

pub(super) fn default_agent_cua_permission_request_permission_status_platform() -> String {
    "darwin".to_owned()
}

pub(super) fn default_agent_cua_permission_request_permission_status_grant_owner() -> String {
    String::new()
}

pub(super) fn fallback_agent_cua_permission_request_permission_status_accessibility()
-> AgentCuaPermissionRequestPermissionStatusAccessibility {
    AgentCuaPermissionRequestPermissionStatusAccessibility::Denied
}

pub(super) fn fallback_agent_cua_permission_request_permission_status_screen_recording()
-> AgentCuaPermissionRequestPermissionStatusScreenRecording {
    AgentCuaPermissionRequestPermissionStatusScreenRecording::Unknown
}

pub(super) fn fallback_agent_exit_reason() -> AgentExitReason {
    AgentExitReason::Exited
}

pub(super) fn default_agent_exit_code() -> Option<i64> {
    None
}

pub(super) fn default_agent_exit_signal() -> Option<String> {
    None
}

pub(super) fn default_agent_state_phase() -> AgentStatePhase {
    AgentStatePhase::Starting
}

pub(super) fn default_agent_state_migration_message() -> String {
    String::new()
}

pub(super) fn default_agent_state_providers() -> Vec<AgentStateProvidersItem> {
    Vec::new()
}

pub(super) fn default_agent_state_host_services() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_agent_state_build() -> String {
    String::new()
}

pub(super) fn fallback_agent_state_storage_phase() -> AgentStateStoragePhase {
    AgentStateStoragePhase::Checking
}

pub(super) fn fallback_agent_state_storage_migration_kind() -> AgentStateStorageMigrationKind {
    AgentStateStorageMigrationKind::None
}

pub(super) fn default_agent_state_diagnostics_version() -> String {
    String::new()
}
