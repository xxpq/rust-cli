//! 排版之前的摘要：0100421cf48dbc26
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::{
    AskedQuestion, CommandResultInputSendDelivery, CommandSessionCreateKind, ElicitationField,
    ElicitationFieldType, InputFateFate, InteractionAnswerPermissionOptionId,
    InteractionHookReviewPayloadDetailSourceFilesItem, InteractionHookReviewPayloadItemsItem,
    InteractionHookReviewPayloadItemsItemTrustState, InteractionMcpElicitationPayloadMode,
    InteractionPermissionPayloadOptionsItem, QuerySessionDebugResultNetworkEntriesItemStatusType,
    QuerySessionFileChangesResultItemsItemPatchesItem,
    QuerySessionSubagentsResultEndedItemsItemStatus, QuerySessionSubagentsResultRunningItemStatus,
    QueryWorkBashOutputResultAlt1Status, QueryWorkBashOutputResultAlt2Kind, QueueItemDispatch,
    QueueItemDispatchState, QueueItemKind, RowAssistantTextFeedback, RowAssistantTextState,
    RowReasoningState, RowTimelineMarkerMarker, RowToolCallStatus, RowTurnHeaderDurations,
    RowTurnHeaderFileChangeSummaryState, RowTurnHeaderOrigin, RowTurnHeaderState, SamplingTurn,
    SamplingTurnRole, ServiceHostCredentialsCipherResultFailedReason,
    ServiceHostPreferencesGetResultModelContextBudgetStrategy,
    ServiceHostPreferencesGetResultToolSearchMode, SessionFactFieldsConfig,
    SessionFactFieldsLastOutcome, SessionFactFieldsTitleSource, SessionStateControlPhase,
    SessionStateControlRuntime,
};
use crate::tolerant::Opaque;
use std::collections::BTreeMap;

pub(super) fn default_command_input_send_attachments() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_command_input_send_context_refs() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_command_session_create_kind() -> CommandSessionCreateKind {
    CommandSessionCreateKind::Main
}

pub(super) fn default_command_session_create_execution_directories() -> Vec<String> {
    Vec::new()
}

pub(super) fn fallback_command_result_input_send_delivery() -> CommandResultInputSendDelivery {
    CommandResultInputSendDelivery::Queue
}

pub(super) fn default_command_result_turn_stop_still_queued() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_command_result_turn_stop_cancelled() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_elicitation_field_required() -> bool {
    false
}

pub(super) fn fallback_elicitation_field_type() -> ElicitationFieldType {
    ElicitationFieldType::Text
}

pub(super) fn fallback_input_fate_fate() -> InputFateFate {
    InputFateFate::Failed
}

pub(super) fn fallback_interaction_answer_permission_option_id()
-> InteractionAnswerPermissionOptionId {
    InteractionAnswerPermissionOptionId::Deny
}

pub(super) fn default_interaction_answer_question_answers() -> BTreeMap<String, Opaque> {
    BTreeMap::new()
}

pub(super) fn default_interaction_permission_payload_summary() -> String {
    String::new()
}

pub(super) fn default_interaction_permission_payload_options()
-> Vec<InteractionPermissionPayloadOptionsItem> {
    vec![
        InteractionPermissionPayloadOptionsItem::AllowOnce,
        InteractionPermissionPayloadOptionsItem::Deny,
    ]
}

pub(super) fn default_interaction_permission_payload_full_access_option() -> bool {
    false
}

pub(super) fn default_interaction_question_payload_questions() -> Vec<AskedQuestion> {
    Vec::new()
}

pub(super) fn default_interaction_hook_review_payload_items_item_event() -> String {
    String::new()
}

pub(super) fn default_interaction_hook_review_payload_items_item_display_name() -> String {
    String::new()
}

pub(super) fn default_interaction_hook_review_payload_items_item_display_command() -> String {
    String::new()
}

pub(super) fn default_interaction_hook_review_payload_items_item_source_path() -> String {
    String::new()
}

pub(super) fn default_interaction_hook_review_payload_items_item_configured_enabled() -> bool {
    false
}

pub(super) fn default_interaction_hook_review_payload_items_item_editable() -> bool {
    false
}

pub(super) fn fallback_interaction_hook_review_payload_items_item_trust_state()
-> InteractionHookReviewPayloadItemsItemTrustState {
    InteractionHookReviewPayloadItemsItemTrustState::BlockedUntrusted
}

pub(super) fn default_interaction_hook_review_payload_items()
-> Vec<InteractionHookReviewPayloadItemsItem> {
    Vec::new()
}

pub(super) fn default_interaction_hook_review_payload_bundle_digest() -> String {
    String::new()
}

pub(super) fn default_interaction_hook_review_payload_detail_workspace_label() -> String {
    String::new()
}

pub(super) fn default_interaction_hook_review_payload_detail_created_at() -> f64 {
    0.0
}

pub(super) fn default_interaction_hook_review_payload_detail_source_files_item_display_path()
-> String {
    String::new()
}

pub(super) fn default_interaction_hook_review_payload_detail_source_files_item_editable() -> bool {
    false
}

pub(super) fn default_interaction_hook_review_payload_detail_source_files()
-> Vec<InteractionHookReviewPayloadDetailSourceFilesItem> {
    Vec::new()
}

pub(super) fn default_interaction_hook_review_payload_detail_summary_event_count() -> u64 {
    0
}

pub(super) fn default_interaction_hook_review_payload_detail_summary_hook_count() -> u64 {
    0
}

pub(super) fn default_interaction_hook_review_payload_detail_summary_pending_count() -> u64 {
    0
}

pub(super) fn default_interaction_hook_review_payload_detail_items()
-> Vec<InteractionHookReviewPayloadItemsItem> {
    Vec::new()
}

pub(super) fn default_interaction_oauth_payload_state() -> String {
    String::new()
}

pub(super) fn default_interaction_mcp_elicitation_payload_message() -> String {
    String::new()
}

pub(super) fn fallback_interaction_mcp_elicitation_payload_mode()
-> InteractionMcpElicitationPayloadMode {
    InteractionMcpElicitationPayloadMode::Form
}

pub(super) fn default_interaction_mcp_elicitation_payload_fields() -> Vec<ElicitationField> {
    Vec::new()
}

pub(super) fn fallback_sampling_turn_role() -> SamplingTurnRole {
    SamplingTurnRole::User
}

pub(super) fn default_interaction_mcp_sampling_payload_messages() -> Vec<SamplingTurn> {
    Vec::new()
}

const DEFAULT_QUERY_ROWS_RANGE_PARAMS_LIMIT: u64 = 200;
pub(super) fn default_query_rows_range_params_limit() -> u64 {
    DEFAULT_QUERY_ROWS_RANGE_PARAMS_LIMIT
}

pub(super) fn default_row_turn_header_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_turn_header_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_turn_header_stable() -> bool {
    true
}

pub(super) fn default_row_turn_header_origin() -> RowTurnHeaderOrigin {
    RowTurnHeaderOrigin::Unknown
}

pub(super) fn default_row_turn_header_execution_kind() -> String {
    String::new()
}

pub(super) fn default_row_turn_header_state() -> RowTurnHeaderState {
    RowTurnHeaderState::Unknown
}

pub(super) fn default_row_turn_header_durations_active_ms() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_durations_total_ms() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_durations() -> RowTurnHeaderDurations {
    RowTurnHeaderDurations {
        active_ms: 0.0,
        total_ms: 0.0,
    }
}

pub(super) fn default_row_turn_header_output_tokens() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_work_segments() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_row_turn_header_file_changes() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_row_turn_header_file_change_summary_additions() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_file_change_summary_deletions() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_file_change_summary_files() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_file_change_summary_state()
-> RowTurnHeaderFileChangeSummaryState {
    RowTurnHeaderFileChangeSummaryState::Active
}

pub(super) fn default_row_turn_header_timing_model_duration_ms() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_timing_tool_duration_ms() -> f64 {
    0.0
}

pub(super) fn default_row_turn_header_timing_other_duration_ms() -> f64 {
    0.0
}

pub(super) fn default_row_user_input_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_user_input_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_user_input_stable() -> bool {
    true
}

pub(super) fn default_row_user_input_text() -> String {
    String::new()
}

pub(super) fn default_row_user_input_origin() -> RowTurnHeaderOrigin {
    RowTurnHeaderOrigin::Unknown
}

pub(super) fn default_row_user_input_guided() -> bool {
    false
}

pub(super) fn default_row_user_input_attachments() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_row_assistant_text_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_assistant_text_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_assistant_text_stable() -> bool {
    true
}

pub(super) fn default_row_assistant_text_text() -> String {
    String::new()
}

pub(super) fn default_row_assistant_text_state() -> RowAssistantTextState {
    RowAssistantTextState::Done
}

pub(super) fn default_row_assistant_text_model() -> String {
    String::new()
}

pub(super) fn default_row_assistant_text_feedback() -> RowAssistantTextFeedback {
    RowAssistantTextFeedback::None
}

pub(super) fn default_row_reasoning_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_reasoning_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_reasoning_stable() -> bool {
    true
}

pub(super) fn default_row_reasoning_text() -> String {
    String::new()
}

pub(super) fn default_row_reasoning_state() -> RowReasoningState {
    RowReasoningState::Done
}

pub(super) fn default_row_reasoning_duration_ms() -> f64 {
    0.0
}

pub(super) fn default_row_tool_call_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_tool_call_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_tool_call_stable() -> bool {
    true
}

pub(super) fn default_row_tool_call_status() -> RowToolCallStatus {
    RowToolCallStatus::Unknown
}

pub(super) fn default_row_tool_call_tool_name() -> String {
    String::new()
}

pub(super) fn default_row_tool_call_input_text() -> String {
    String::new()
}

pub(super) fn default_row_tool_call_backgrounded() -> bool {
    false
}

pub(super) fn default_row_subagent_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_subagent_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_subagent_stable() -> bool {
    true
}

pub(super) fn default_row_subagent_parent_tool_call_id() -> String {
    String::new()
}

pub(super) fn default_row_subagent_status() -> RowTurnHeaderState {
    RowTurnHeaderState::Unknown
}

pub(super) fn default_row_subagent_summary_text() -> String {
    String::new()
}

pub(super) fn default_row_hook_invocation_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_hook_invocation_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_hook_invocation_stable() -> bool {
    true
}

pub(super) fn default_row_hook_invocation_hook_event_name() -> String {
    String::new()
}

pub(super) fn default_row_hook_invocation_lane() -> String {
    String::new()
}

pub(super) fn default_row_hook_invocation_executions() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_row_timeline_marker_turn_id() -> String {
    String::new()
}

pub(super) fn default_row_timeline_marker_created_at_seq() -> u64 {
    0
}

pub(super) fn default_row_timeline_marker_stable() -> bool {
    true
}

pub(super) fn default_row_timeline_marker_marker() -> RowTimelineMarkerMarker {
    RowTimelineMarkerMarker::Unknown
}

pub(super) fn fallback_query_session_debug_result_network_entries_item_status_type()
-> QuerySessionDebugResultNetworkEntriesItemStatusType {
    QuerySessionDebugResultNetworkEntriesItemStatusType::ModelRequestStarted
}

pub(super) fn default_query_session_debug_result_network_entries_item_request_headers()
-> BTreeMap<String, String> {
    BTreeMap::new()
}

pub(super) fn default_query_session_debug_result_network_entries_item_response_headers()
-> BTreeMap<String, String> {
    BTreeMap::new()
}

pub(super) fn default_query_session_file_changes_result_items_item_tool_names() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_query_session_file_changes_result_items_item_patches()
-> Vec<QuerySessionFileChangesResultItemsItemPatchesItem> {
    Vec::new()
}

pub(super) fn default_query_session_prompts_result_items_item_answer() -> Vec<String> {
    Vec::new()
}

const DEFAULT_QUERY_SESSION_SUBAGENTS_PARAMS_ENDED_LIMIT: u64 = 20;
pub(super) fn default_query_session_subagents_params_ended_limit() -> u64 {
    DEFAULT_QUERY_SESSION_SUBAGENTS_PARAMS_ENDED_LIMIT
}

pub(super) fn fallback_query_session_subagents_result_running_item_status()
-> QuerySessionSubagentsResultRunningItemStatus {
    QuerySessionSubagentsResultRunningItemStatus::Running
}

pub(super) fn fallback_query_session_subagents_result_ended_items_item_status()
-> QuerySessionSubagentsResultEndedItemsItemStatus {
    QuerySessionSubagentsResultEndedItemsItemStatus::Lost
}

pub(super) fn default_session_fact_fields_title() -> String {
    String::new()
}

pub(super) fn default_session_fact_fields_title_source() -> SessionFactFieldsTitleSource {
    SessionFactFieldsTitleSource::Default
}

pub(super) fn default_session_fact_fields_created_at() -> f64 {
    0.0
}

pub(super) fn default_session_fact_fields_updated_at() -> f64 {
    0.0
}

pub(super) fn default_session_fact_fields_last_outcome() -> Option<SessionFactFieldsLastOutcome> {
    None
}

pub(super) fn default_session_fact_fields_deleted_at() -> Option<f64> {
    None
}

pub(super) fn default_session_fact_fields_config_model() -> String {
    String::new()
}

pub(super) fn default_session_fact_fields_config_mode() -> String {
    String::new()
}

pub(super) fn default_session_fact_fields_config_thought() -> String {
    String::new()
}

pub(super) fn default_session_fact_fields_config() -> SessionFactFieldsConfig {
    SessionFactFieldsConfig {
        model: default_session_fact_fields_config_model(),
        mode: default_session_fact_fields_config_mode(),
        thought: default_session_fact_fields_config_thought(),
    }
}

pub(super) fn fallback_query_work_bash_output_result_alt1_status()
-> QueryWorkBashOutputResultAlt1Status {
    QueryWorkBashOutputResultAlt1Status::Running
}

pub(super) fn fallback_query_work_bash_output_result_alt2_kind() -> QueryWorkBashOutputResultAlt2Kind
{
    QueryWorkBashOutputResultAlt2Kind::Unavailable
}

pub(super) fn default_queue_item_kind() -> QueueItemKind {
    QueueItemKind::Input
}

pub(super) fn default_queue_item_text() -> String {
    String::new()
}

pub(super) fn default_queue_item_attachments() -> Vec<Opaque> {
    Vec::new()
}

pub(super) fn default_queue_item_dispatch_state() -> QueueItemDispatchState {
    QueueItemDispatchState::Waiting
}

pub(super) fn default_queue_item_dispatch() -> QueueItemDispatch {
    QueueItemDispatch {
        state: QueueItemDispatchState::Waiting,
        reservation_id: None,
    }
}

pub(super) fn default_queue_item_order_admission_seq() -> u64 {
    0
}

pub(super) fn fallback_service_host_credentials_cipher_result_failed_reason()
-> ServiceHostCredentialsCipherResultFailedReason {
    ServiceHostCredentialsCipherResultFailedReason::Unavailable
}

pub(super) fn default_service_host_preferences_get_result_native_search_enhancements_enabled()
-> bool {
    true
}

pub(super) fn default_service_host_preferences_get_result_memory_enabled() -> bool {
    false
}

pub(super) fn default_service_host_preferences_get_result_tool_search_mode()
-> ServiceHostPreferencesGetResultToolSearchMode {
    ServiceHostPreferencesGetResultToolSearchMode::Off
}

pub(super) fn default_service_host_preferences_get_result_ask_user_question_auto_resolution_enabled()
-> bool {
    true
}

pub(super) fn default_service_host_preferences_get_result_model_context_budget_strategy()
-> ServiceHostPreferencesGetResultModelContextBudgetStrategy {
    ServiceHostPreferencesGetResultModelContextBudgetStrategy::PreflightV1
}

pub(super) fn default_session_fact_title() -> String {
    String::new()
}

pub(super) fn default_session_fact_title_source() -> SessionFactFieldsTitleSource {
    SessionFactFieldsTitleSource::Default
}

pub(super) fn default_session_fact_created_at() -> f64 {
    0.0
}

pub(super) fn default_session_fact_updated_at() -> f64 {
    0.0
}

pub(super) fn default_session_fact_last_outcome() -> Option<SessionFactFieldsLastOutcome> {
    None
}

pub(super) fn default_session_fact_deleted_at() -> Option<f64> {
    None
}

pub(super) fn default_session_fact_config() -> SessionFactFieldsConfig {
    SessionFactFieldsConfig {
        model: default_session_fact_fields_config_model(),
        mode: default_session_fact_fields_config_mode(),
        thought: default_session_fact_fields_config_thought(),
    }
}

pub(super) fn default_session_state_control_phase() -> SessionStateControlPhase {
    SessionStateControlPhase::Idle
}

pub(super) fn default_session_state_control_runtime() -> SessionStateControlRuntime {
    SessionStateControlRuntime::Cold
}

pub(super) fn default_session_state_control_turn_id() -> Option<String> {
    None
}
