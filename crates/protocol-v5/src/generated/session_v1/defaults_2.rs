//! 排版之前的摘要：a53c0415fef17bb3
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_session_state_control_phase, default_session_state_control_runtime,
    default_session_state_control_turn_id,
};
use super::{
    SessionFactFieldsTitleSource, SessionStateAdmission, SessionStateAdmissionInput,
    SessionStateAdmissionInputAllowedItem, SessionStateAdmissionInputMode,
    SessionStateAdmissionStop, SessionStateConfig, SessionStateConfigFollowupMode,
    SessionStateControl, SessionStateControlActiveWorksItem,
    SessionStateControlActiveWorksItemKind, SessionStateControlApiRetry,
    SessionStateControlLastError, SessionStateControlLastOutcome, SessionStateGoal,
    SessionStateGoalIterationsItem, SessionStateGoalStatus, SessionStateHookAdmission,
    SessionStateMeta, SessionStateMetaKind, SessionStateNoticesItem, SessionStateNoticesItemLevel,
    SessionStatePlan, SessionStatePlanItemsItem, SessionStatePlanItemsItemStatus,
    SessionStateQueuePolicy, SessionStateRowsWindow, SessionStateSharedContext,
    SessionStateSharedContextStatus, SessionStateUsage, SessionStateUsageContextWindow,
    SessionStateUsageCumulative, WorkKind, WorkStatus,
};
use crate::tolerant::Opaque;

pub(super) fn fallback_session_state_control_active_works_item_kind()
-> SessionStateControlActiveWorksItemKind {
    SessionStateControlActiveWorksItemKind::Unknown
}

pub(super) fn default_session_state_control_active_works_item_started_at_seq() -> u64 {
    0
}

pub(super) fn default_session_state_control_active_works() -> Vec<SessionStateControlActiveWorksItem>
{
    Vec::new()
}

pub(super) fn default_session_state_control_last_error_message() -> String {
    String::new()
}

pub(super) fn default_session_state_control_last_error() -> Option<SessionStateControlLastError> {
    None
}

pub(super) fn default_session_state_control_api_retry_reason() -> String {
    String::new()
}

pub(super) fn default_session_state_control_api_retry() -> Option<SessionStateControlApiRetry> {
    None
}

pub(super) fn default_session_state_control_last_outcome() -> Option<SessionStateControlLastOutcome>
{
    None
}

pub(super) fn default_session_state_control() -> SessionStateControl {
    SessionStateControl {
        phase: default_session_state_control_phase(),
        runtime: default_session_state_control_runtime(),
        turn_id: default_session_state_control_turn_id(),
        active_works: default_session_state_control_active_works(),
        last_error: default_session_state_control_last_error(),
        api_retry: default_session_state_control_api_retry(),
        last_outcome: default_session_state_control_last_outcome(),
    }
}

pub(super) fn default_session_state_meta_title() -> String {
    String::new()
}

pub(super) fn default_session_state_meta_title_source() -> SessionFactFieldsTitleSource {
    SessionFactFieldsTitleSource::Default
}

pub(super) fn default_session_state_meta_created_at() -> f64 {
    0.0
}

pub(super) fn default_session_state_meta_kind() -> SessionStateMetaKind {
    SessionStateMetaKind::Main
}

pub(super) fn default_session_state_meta() -> SessionStateMeta {
    SessionStateMeta {
        title: default_session_state_meta_title(),
        title_source: default_session_state_meta_title_source(),
        created_at: default_session_state_meta_created_at(),
        kind: default_session_state_meta_kind(),
    }
}

pub(super) fn default_session_state_config_provider() -> String {
    String::new()
}

pub(super) fn default_session_state_config_model() -> String {
    String::new()
}

pub(super) fn default_session_state_config_thought() -> String {
    String::new()
}

pub(super) fn default_session_state_config_mode() -> String {
    "default".to_owned()
}

pub(super) fn default_session_state_config_followup_mode() -> SessionStateConfigFollowupMode {
    SessionStateConfigFollowupMode::Queue
}

pub(super) fn default_session_state_config_plan_enabled() -> bool {
    false
}

pub(super) fn default_session_state_config() -> SessionStateConfig {
    SessionStateConfig {
        provider: default_session_state_config_provider(),
        model: default_session_state_config_model(),
        thought: default_session_state_config_thought(),
        mode: default_session_state_config_mode(),
        followup_mode: default_session_state_config_followup_mode(),
        plan_enabled: default_session_state_config_plan_enabled(),
        model_selection: None,
        fallback: None,
        permission_grant: None,
        plan_transition: None,
    }
}

pub(super) fn default_session_state_usage_context_window() -> Option<SessionStateUsageContextWindow>
{
    None
}

pub(super) fn default_session_state_usage_cumulative_input_tokens() -> f64 {
    0.0
}

pub(super) fn default_session_state_usage_cumulative_output_tokens() -> f64 {
    0.0
}

pub(super) fn default_session_state_usage_cumulative_cache_read() -> f64 {
    0.0
}

pub(super) fn default_session_state_usage_cumulative_cache_write() -> f64 {
    0.0
}

pub(super) fn default_session_state_usage_cumulative() -> SessionStateUsageCumulative {
    SessionStateUsageCumulative {
        input_tokens: 0.0,
        output_tokens: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
    }
}

pub(super) fn default_session_state_usage() -> SessionStateUsage {
    SessionStateUsage {
        context_window: default_session_state_usage_context_window(),
        cumulative: default_session_state_usage_cumulative(),
    }
}

pub(super) fn default_session_state_goal_target_id() -> Option<String> {
    None
}

pub(super) fn default_session_state_goal_status() -> SessionStateGoalStatus {
    SessionStateGoalStatus::None
}

pub(super) fn default_session_state_goal_iteration() -> u64 {
    0
}

pub(super) fn default_session_state_goal_verifications() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_session_state_goal_iterations() -> Vec<SessionStateGoalIterationsItem> {
    Vec::new()
}

pub(super) fn default_session_state_goal() -> SessionStateGoal {
    SessionStateGoal {
        target_id: default_session_state_goal_target_id(),
        status: default_session_state_goal_status(),
        iteration: default_session_state_goal_iteration(),
        verifications: default_session_state_goal_verifications(),
        iterations: default_session_state_goal_iterations(),
        objective: None,
        summary_title: None,
        time_used_seconds: None,
        active_run_started_at_ms: None,
    }
}

pub(super) fn default_session_state_plan_items_item_text() -> String {
    String::new()
}

pub(super) fn default_session_state_plan_items_item_status() -> SessionStatePlanItemsItemStatus {
    SessionStatePlanItemsItemStatus::Pending
}

pub(super) fn default_session_state_plan_items() -> Vec<SessionStatePlanItemsItem> {
    Vec::new()
}

pub(super) fn default_session_state_plan_updated_at() -> f64 {
    0.0
}

pub(super) fn default_session_state_plan() -> SessionStatePlan {
    SessionStatePlan {
        items: default_session_state_plan_items(),
        updated_at: default_session_state_plan_updated_at(),
    }
}

pub(super) fn default_session_state_shared_context_title() -> String {
    String::new()
}

pub(super) fn default_session_state_shared_context_status() -> SessionStateSharedContextStatus {
    SessionStateSharedContextStatus::Pending
}

pub(super) fn default_session_state_shared_context() -> Option<SessionStateSharedContext> {
    None
}

pub(super) fn default_session_state_queue_policy_auto_drain() -> bool {
    true
}

pub(super) fn default_session_state_queue_policy() -> SessionStateQueuePolicy {
    SessionStateQueuePolicy {
        auto_drain: default_session_state_queue_policy_auto_drain(),
        pause_reason: None,
    }
}

pub(super) fn default_session_state_admission_input_mode() -> SessionStateAdmissionInputMode {
    SessionStateAdmissionInputMode::StartNow
}

pub(super) fn default_session_state_admission_input_allowed()
-> Vec<SessionStateAdmissionInputAllowedItem> {
    Vec::new()
}

pub(super) fn default_session_state_admission_input() -> SessionStateAdmissionInput {
    SessionStateAdmissionInput {
        mode: SessionStateAdmissionInputMode::StartNow,
        reason: None,
        allowed: Vec::new(),
    }
}

pub(super) fn default_session_state_admission_stop() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_compact() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_fork() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_edit() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_retry() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_rewind_files() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_queue_send_now() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_goal_pause() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission_goal_resume() -> SessionStateAdmissionStop {
    SessionStateAdmissionStop {
        allow: true,
        reason: None,
    }
}

pub(super) fn default_session_state_admission() -> SessionStateAdmission {
    SessionStateAdmission {
        input: default_session_state_admission_input(),
        stop: default_session_state_admission_stop(),
        compact: default_session_state_admission_compact(),
        fork: default_session_state_admission_fork(),
        edit: default_session_state_admission_edit(),
        retry: default_session_state_admission_retry(),
        rewind_files: default_session_state_admission_rewind_files(),
        queue_send_now: default_session_state_admission_queue_send_now(),
        goal_pause: default_session_state_admission_goal_pause(),
        goal_resume: default_session_state_admission_goal_resume(),
    }
}

pub(super) fn default_session_state_hook_admission_bundle_digest() -> String {
    String::new()
}

pub(super) fn default_session_state_hook_admission() -> Option<SessionStateHookAdmission> {
    None
}

pub(super) fn default_session_state_mcp_admission() -> Option<SessionStateHookAdmission> {
    None
}

pub(super) fn default_session_state_rows_window_total_count() -> u64 {
    0
}

pub(super) fn default_session_state_rows_window_first_id() -> Option<String> {
    None
}

pub(super) fn default_session_state_rows_window_earlier() -> bool {
    false
}

pub(super) fn default_session_state_rows_window() -> SessionStateRowsWindow {
    SessionStateRowsWindow {
        total_count: default_session_state_rows_window_total_count(),
        first_id: default_session_state_rows_window_first_id(),
        earlier: default_session_state_rows_window_earlier(),
    }
}

pub(super) fn default_session_state_queue_order() -> Vec<String> {
    Vec::new()
}

pub(super) fn fallback_session_state_notices_item_level() -> SessionStateNoticesItemLevel {
    SessionStateNoticesItemLevel::Info
}

pub(super) fn default_session_state_notices() -> Vec<SessionStateNoticesItem> {
    Vec::new()
}

pub(super) fn fallback_work_kind() -> WorkKind {
    WorkKind::Bash
}

pub(super) fn default_work_status() -> WorkStatus {
    WorkStatus::Running
}

pub(super) fn default_work_title() -> String {
    String::new()
}

pub(super) fn default_work_started_at_seq() -> u64 {
    0
}
