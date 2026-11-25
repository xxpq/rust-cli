//! 排版之前的摘要：af73e20bd7e62aaf
//! 生成物，不要手改：`core@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::{
    CommandResultAcceptedEffectsItem, Cursor, ExitReason, InitializeParamsPeer,
    InitializeParamsPeerBuild, InitializeParamsPeerKind, InitializeRejectionUpgradeTarget,
    InitializeResultDelivery, InitializeResultPrincipal, InitializeResultServer,
};
use crate::tolerant::Opaque;
use serde::Deserializer;
use std::collections::BTreeMap;

pub(super) fn default_command_envelope_payload() -> Opaque {
    serde_json::json!({})
}

pub(super) fn default_command_result_accepted_effects() -> Vec<CommandResultAcceptedEffectsItem> {
    Vec::new()
}

pub(super) fn default_command_result_accepted_noop() -> bool {
    false
}

pub(super) fn default_credit_n() -> u64 {
    1
}

pub(super) fn fallback_exit_reason() -> ExitReason {
    ExitReason::Unknown
}

pub(super) fn default_snapshot_state() -> BTreeMap<String, Opaque> {
    BTreeMap::new()
}

pub(super) fn default_snapshot_collections() -> BTreeMap<String, Vec<Opaque>> {
    BTreeMap::new()
}

pub(super) fn default_initialize_params_peer_kind() -> InitializeParamsPeerKind {
    InitializeParamsPeerKind::Unknown
}

pub(super) fn default_initialize_params_peer_name() -> String {
    String::new()
}

pub(super) fn default_initialize_params_peer_build_version() -> String {
    String::new()
}

pub(super) fn default_initialize_params_peer_build() -> InitializeParamsPeerBuild {
    InitializeParamsPeerBuild {
        version: String::new(),
        commit: None,
    }
}

pub(super) fn default_initialize_params_peer() -> InitializeParamsPeer {
    InitializeParamsPeer {
        kind: InitializeParamsPeerKind::Unknown,
        name: String::new(),
        build: InitializeParamsPeerBuild {
            version: String::new(),
            commit: None,
        },
    }
}

pub(super) fn default_initialize_params_require() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_initialize_rejection_missing_item_mine() -> Vec<i64> {
    Vec::new()
}

pub(super) fn default_initialize_rejection_missing_item_theirs() -> Vec<i64> {
    Vec::new()
}

pub(super) fn fallback_initialize_rejection_upgrade_target() -> InitializeRejectionUpgradeTarget {
    InitializeRejectionUpgradeTarget::Both
}

pub(super) fn default_initialize_rejection_upgrade_message() -> String {
    String::new()
}

pub(super) fn fallback_initialize_result_principal() -> InitializeResultPrincipal {
    InitializeResultPrincipal::Unknown
}

pub(super) fn fallback_initialize_result_delivery() -> InitializeResultDelivery {
    InitializeResultDelivery::Replayable
}

pub(super) fn default_initialize_result_server_kind() -> String {
    "host".to_owned()
}

pub(super) fn default_initialize_result_server_build() -> InitializeParamsPeerBuild {
    InitializeParamsPeerBuild {
        version: String::new(),
        commit: None,
    }
}

pub(super) fn default_initialize_result_server() -> InitializeResultServer {
    InitializeResultServer {
        kind: "host".to_owned(),
        build: InitializeParamsPeerBuild {
            version: String::new(),
            commit: None,
        },
        host_id: None,
    }
}

pub(super) fn fallback_protocol_error_message() -> String {
    String::new()
}

pub(super) fn catch_protocol_error_message<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    crate::tolerant::lenient(deserializer, fallback_protocol_error_message)
}

pub(super) fn fallback_protocol_error_retryable() -> bool {
    false
}

pub(super) fn catch_protocol_error_retryable<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    crate::tolerant::lenient(deserializer, fallback_protocol_error_retryable)
}

pub(super) fn default_subscribe_cursor() -> Option<Cursor> {
    None
}
