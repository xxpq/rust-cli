//! Node 用量表的写入与聚合（Node `repositories/usage.ts`；本机实现 node-store `repo/usage.rs`）。冲突时的合并规则逐列
//! 照搬，冲突键与过滤都带 tenant_id；SQLite 的两参数 `min` / `max` 换成 PG 的 `least` / `greatest`。
use rust_cli_node_records::rows::{ModelRow, ToolRow, TurnRow, TurnTotals};
use sea_orm::sea_query::{Alias, Expr, Func, OnConflict};
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::PgSessionStore;
use crate::entities::{model_usage, tool_usage, turn_usage};
use crate::transaction::Tx;

/// Node `recordModelUsage`（不含它之后的 `pruneUsage`，见 [`prune`]）。
pub(crate) async fn record_model(
    store: &PgSessionStore,
    tx: &Tx,
    row: &ModelRow,
) -> Result<(), DbErr> {
    model_usage::Entity::insert(model_usage::ActiveModel {
        tenant_id: Set(store.tenant().to_owned()),
        id: Set(row.id.clone()),
        logical_request_id: Set(row.logical_request_id.clone()),
        attempt_index: Set(row.attempt_index),
        session_id: Set(row.session_id.clone()),
        turn_id: Set(row.turn_id.clone()),
        trace_id: Set(row.trace_id.clone()),
        span_id: Set(row.span_id.clone()),
        assistant_message_id: Set(row.assistant_message_id.clone()),
        parent_user_message_id: Set(row.parent_user_message_id.clone()),
        query_source: Set(row.query_source.clone()),
        provider_id: Set(row.provider_id.clone()),
        model_id: Set(row.model_id.clone()),
        variant: Set(row.variant.clone()),
        agent: Set(row.agent.clone()),
        mode: Set(row.mode.clone()),
        task_type: Set(row.task_type.clone()),
        status: Set(row.status.to_owned()),
        started_at: Set(row.started_at),
        first_token_at: Set(row.first_token_at),
        completed_at: Set(row.completed_at),
        duration_ms: Set(row.duration_ms),
        time_to_first_token_ms: Set(row.time_to_first_token_ms),
        finish_reason: Set(row.finish_reason.map(str::to_owned)),
        tool_call_count: Set(row.tool_call_count),
        input_tokens: Set(row.input_tokens),
        output_tokens: Set(row.output_tokens),
        reasoning_tokens: Set(row.reasoning_tokens),
        cache_creation_input_tokens: Set(row.cache_creation_input_tokens),
        cache_read_input_tokens: Set(row.cache_read_input_tokens),
        provider_total_tokens: Set(row.provider_total_tokens),
        computed_total_tokens: Set(row.computed_total_tokens),
        retry_count: Set(row.retry_count),
        retryable: Set(i64::from(row.retryable)),
        cancelled_by_user: Set(i64::from(row.cancelled_by_user)),
        context_exceeded: Set(i64::from(row.context_exceeded)),
        error_type: Set(row.error_type.clone()),
        error_code: Set(row.error_code.clone()),
        error_message: Set(row.error_message.clone()),
        raw_usage_json: Set(row.raw_usage_json.clone()),
        provider_metadata_json: Set(row.provider_metadata_json.clone()),
    })
    .on_conflict(
        OnConflict::columns([model_usage::Column::TenantId, model_usage::Column::Id])
            .value(
                model_usage::Column::LogicalRequestId,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::LogicalRequestId,
                )),
            )
            .value(
                model_usage::Column::AttemptIndex,
                Expr::col((Alias::new("excluded"), model_usage::Column::AttemptIndex)),
            )
            .value(
                model_usage::Column::SessionId,
                Expr::col((Alias::new("excluded"), model_usage::Column::SessionId)),
            )
            .value(
                model_usage::Column::TurnId,
                Expr::col((Alias::new("excluded"), model_usage::Column::TurnId)),
            )
            .value(
                model_usage::Column::TraceId,
                Expr::col((Alias::new("excluded"), model_usage::Column::TraceId)),
            )
            .value(
                model_usage::Column::SpanId,
                Expr::col((Alias::new("excluded"), model_usage::Column::SpanId)),
            )
            .value(
                model_usage::Column::AssistantMessageId,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::AssistantMessageId,
                )),
            )
            .value(
                model_usage::Column::ParentUserMessageId,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::ParentUserMessageId,
                )),
            )
            .value(
                model_usage::Column::QuerySource,
                Expr::col((Alias::new("excluded"), model_usage::Column::QuerySource)),
            )
            .value(
                model_usage::Column::ProviderId,
                Expr::col((Alias::new("excluded"), model_usage::Column::ProviderId)),
            )
            .value(
                model_usage::Column::ModelId,
                Expr::col((Alias::new("excluded"), model_usage::Column::ModelId)),
            )
            .value(
                model_usage::Column::Variant,
                Expr::col((Alias::new("excluded"), model_usage::Column::Variant)),
            )
            .value(
                model_usage::Column::Agent,
                Expr::col((Alias::new("excluded"), model_usage::Column::Agent)),
            )
            .value(
                model_usage::Column::Mode,
                Expr::col((Alias::new("excluded"), model_usage::Column::Mode)),
            )
            .value(
                model_usage::Column::TaskType,
                Expr::col((Alias::new("excluded"), model_usage::Column::TaskType)),
            )
            .value(
                model_usage::Column::Status,
                Expr::col((Alias::new("excluded"), model_usage::Column::Status)),
            )
            .value(
                model_usage::Column::StartedAt,
                Expr::col((Alias::new("excluded"), model_usage::Column::StartedAt)),
            )
            .value(
                model_usage::Column::FirstTokenAt,
                Expr::col((Alias::new("excluded"), model_usage::Column::FirstTokenAt)),
            )
            .value(
                model_usage::Column::CompletedAt,
                Expr::col((Alias::new("excluded"), model_usage::Column::CompletedAt)),
            )
            .value(
                model_usage::Column::DurationMs,
                Expr::col((Alias::new("excluded"), model_usage::Column::DurationMs)),
            )
            .value(
                model_usage::Column::TimeToFirstTokenMs,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::TimeToFirstTokenMs,
                )),
            )
            .value(
                model_usage::Column::FinishReason,
                Expr::col((Alias::new("excluded"), model_usage::Column::FinishReason)),
            )
            .value(
                model_usage::Column::ToolCallCount,
                Expr::col((Alias::new("excluded"), model_usage::Column::ToolCallCount)),
            )
            .value(
                model_usage::Column::InputTokens,
                Expr::col((Alias::new("excluded"), model_usage::Column::InputTokens)),
            )
            .value(
                model_usage::Column::OutputTokens,
                Expr::col((Alias::new("excluded"), model_usage::Column::OutputTokens)),
            )
            .value(
                model_usage::Column::ReasoningTokens,
                Expr::col((Alias::new("excluded"), model_usage::Column::ReasoningTokens)),
            )
            .value(
                model_usage::Column::CacheCreationInputTokens,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::CacheCreationInputTokens,
                )),
            )
            .value(
                model_usage::Column::CacheReadInputTokens,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::CacheReadInputTokens,
                )),
            )
            .value(
                model_usage::Column::ProviderTotalTokens,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::ProviderTotalTokens,
                )),
            )
            .value(
                model_usage::Column::ComputedTotalTokens,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::ComputedTotalTokens,
                )),
            )
            .value(
                model_usage::Column::RetryCount,
                Expr::col((Alias::new("excluded"), model_usage::Column::RetryCount)),
            )
            .value(
                model_usage::Column::Retryable,
                Expr::col((Alias::new("excluded"), model_usage::Column::Retryable)),
            )
            .value(
                model_usage::Column::CancelledByUser,
                Expr::col((Alias::new("excluded"), model_usage::Column::CancelledByUser)),
            )
            .value(
                model_usage::Column::ContextExceeded,
                Expr::col((Alias::new("excluded"), model_usage::Column::ContextExceeded)),
            )
            .value(
                model_usage::Column::ErrorType,
                Expr::col((Alias::new("excluded"), model_usage::Column::ErrorType)),
            )
            .value(
                model_usage::Column::ErrorCode,
                Expr::col((Alias::new("excluded"), model_usage::Column::ErrorCode)),
            )
            .value(
                model_usage::Column::ErrorMessage,
                Expr::col((Alias::new("excluded"), model_usage::Column::ErrorMessage)),
            )
            .value(
                model_usage::Column::RawUsageJson,
                Expr::col((Alias::new("excluded"), model_usage::Column::RawUsageJson)),
            )
            .value(
                model_usage::Column::ProviderMetadataJson,
                Expr::col((
                    Alias::new("excluded"),
                    model_usage::Column::ProviderMetadataJson,
                )),
            )
            .to_owned(),
    )
    .exec(tx)
    .await?;
    Ok(())
}

/// Node `upsertToolUsage`。本工程没有的列（副作用范围、只读、破坏性、退出码、stdout / stderr 字节、错误）
/// 绑定 Node 在缺省时的值（usage.md §2.3、D6）。
pub(crate) async fn upsert_tool(
    store: &PgSessionStore,
    tx: &Tx,
    row: &ToolRow,
) -> Result<(), DbErr> {
    tool_usage::Entity::insert(tool_usage::ActiveModel {
        tenant_id: Set(store.tenant().to_owned()),
        id: Set(row.id.clone()),
        session_id: Set(row.session_id.clone()),
        turn_id: Set(row.turn_id.clone()),
        trace_id: Set(row.trace_id.clone()),
        tool_call_id: Set(row.tool_call_id.clone()),
        tool_name: Set(row.tool_name.clone()),
        side_effect_scope: Set(None::<String>),
        read_only: Set(None::<i64>),
        destructive: Set(None::<i64>),
        approval_status: Set(row.approval_status.map(str::to_owned)),
        status: Set(row.status .to_owned()),
        started_at: Set(row.started_at),
        first_output_at: Set(row.first_output_at),
        completed_at: Set(row.completed_at),
        duration_ms: Set(row.duration_ms),
        time_to_first_output_ms: Set(row.time_to_first_output_ms),
        exit_code: Set(None::<i64>),
        output_bytes: Set(row.output_bytes),
        stdout_bytes: Set(0_i64),
        stderr_bytes: Set(0_i64),
        truncated: Set(0_i64),
        retry_count: Set(0_i64),
        retryable: Set(0_i64),
        cancelled_by_user: Set(i64::from(row.cancelled_by_user)),
        error_type: Set(None::<String>),
        error_code: Set(None::<String>),
        error_message: Set(None::<String>),
    }).on_conflict(OnConflict::columns([tool_usage::Column::TenantId, tool_usage::Column::Id])
        .value(tool_usage::Column::SessionId, Expr::col((Alias::new("excluded"), tool_usage::Column::SessionId)))
        .value(tool_usage::Column::TurnId, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::TurnId)), Expr::col((tool_usage::Entity, tool_usage::Column::TurnId))])))
        .value(tool_usage::Column::TraceId, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::TraceId)), Expr::col((tool_usage::Entity, tool_usage::Column::TraceId))])))
        .value(tool_usage::Column::ToolCallId, Expr::col((Alias::new("excluded"), tool_usage::Column::ToolCallId)))
        .value(tool_usage::Column::ToolName, Expr::cust("case when excluded.tool_name = 'unknown' then tool_usage.tool_name else excluded.tool_name end"))
        .value(tool_usage::Column::SideEffectScope, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::SideEffectScope)), Expr::col((tool_usage::Entity, tool_usage::Column::SideEffectScope))])))
        .value(tool_usage::Column::ReadOnly, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::ReadOnly)), Expr::col((tool_usage::Entity, tool_usage::Column::ReadOnly))])))
        .value(tool_usage::Column::Destructive, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::Destructive)), Expr::col((tool_usage::Entity, tool_usage::Column::Destructive))])))
        .value(tool_usage::Column::ApprovalStatus, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::ApprovalStatus)), Expr::col((tool_usage::Entity, tool_usage::Column::ApprovalStatus))])))
        .value(tool_usage::Column::Status, Expr::cust("case when tool_usage.status in ('completed', 'error', 'cancelled') and excluded.status = 'running' then tool_usage.status else excluded.status end"))
        .value(tool_usage::Column::StartedAt, Expr::expr(Func::cust(Alias::new("least")).args([Expr::col((tool_usage::Entity, tool_usage::Column::StartedAt)), Expr::col((Alias::new("excluded"), tool_usage::Column::StartedAt))])))
        .value(tool_usage::Column::FirstOutputAt, Expr::expr(Func::coalesce([Expr::col((tool_usage::Entity, tool_usage::Column::FirstOutputAt)), Expr::col((Alias::new("excluded"), tool_usage::Column::FirstOutputAt))])))
        .value(tool_usage::Column::CompletedAt, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::CompletedAt)), Expr::col((tool_usage::Entity, tool_usage::Column::CompletedAt))])))
        .value(tool_usage::Column::DurationMs, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::DurationMs)), Expr::col((tool_usage::Entity, tool_usage::Column::DurationMs))])))
        .value(tool_usage::Column::TimeToFirstOutputMs, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::TimeToFirstOutputMs)), Expr::col((tool_usage::Entity, tool_usage::Column::TimeToFirstOutputMs))])))
        .value(tool_usage::Column::ExitCode, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::ExitCode)), Expr::col((tool_usage::Entity, tool_usage::Column::ExitCode))])))
        .value(tool_usage::Column::OutputBytes, Expr::expr(Func::cust(Alias::new("greatest")).args([Expr::col((tool_usage::Entity, tool_usage::Column::OutputBytes)), Expr::col((Alias::new("excluded"), tool_usage::Column::OutputBytes))])))
        .value(tool_usage::Column::StdoutBytes, Expr::expr(Func::cust(Alias::new("greatest")).args([Expr::col((tool_usage::Entity, tool_usage::Column::StdoutBytes)), Expr::col((Alias::new("excluded"), tool_usage::Column::StdoutBytes))])))
        .value(tool_usage::Column::StderrBytes, Expr::expr(Func::cust(Alias::new("greatest")).args([Expr::col((tool_usage::Entity, tool_usage::Column::StderrBytes)), Expr::col((Alias::new("excluded"), tool_usage::Column::StderrBytes))])))
        .value(tool_usage::Column::Truncated, Expr::expr(Func::cust(Alias::new("greatest")).args([Expr::col((tool_usage::Entity, tool_usage::Column::Truncated)), Expr::col((Alias::new("excluded"), tool_usage::Column::Truncated))])))
        .value(tool_usage::Column::RetryCount, Expr::col((Alias::new("excluded"), tool_usage::Column::RetryCount)))
        .value(tool_usage::Column::Retryable, Expr::col((Alias::new("excluded"), tool_usage::Column::Retryable)))
        .value(tool_usage::Column::CancelledByUser, Expr::col((Alias::new("excluded"), tool_usage::Column::CancelledByUser)))
        .value(tool_usage::Column::ErrorType, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::ErrorType)), Expr::col((tool_usage::Entity, tool_usage::Column::ErrorType))])))
        .value(tool_usage::Column::ErrorCode, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::ErrorCode)), Expr::col((tool_usage::Entity, tool_usage::Column::ErrorCode))])))
        .value(tool_usage::Column::ErrorMessage, Expr::expr(Func::coalesce([Expr::col((Alias::new("excluded"), tool_usage::Column::ErrorMessage)), Expr::col((tool_usage::Entity, tool_usage::Column::ErrorMessage))])))
        .to_owned()).exec(tx).await?;
    Ok(())
}

/// Node `upsertTurnUsage`（`retryable`、错误列绑定缺省值，usage.md §2.4）。
pub(crate) async fn upsert_turn(
    store: &PgSessionStore,
    tx: &Tx,
    row: &TurnRow,
) -> Result<(), DbErr> {
    turn_usage::Entity::insert(turn_usage::ActiveModel {
        tenant_id: Set(store.tenant().to_owned()),
        session_id: Set(row.session_id.clone()),
        turn_id: Set(row.turn_id.clone()),
        trace_id: Set(row.trace_id.clone()),
        user_message_id: Set(row.user_message_id.clone()),
        status: Set(row.status.to_owned()),
        started_at: Set(row.started_at),
        first_model_start_at: Set(row.first_model_start_at),
        first_token_at: Set(row.first_token_at),
        completed_at: Set(row.completed_at),
        duration_ms: Set(row.duration_ms),
        time_to_first_token_ms: Set(row.time_to_first_token_ms),
        model_request_count: Set(row.model_request_count),
        model_retry_count: Set(row.model_retry_count),
        tool_call_count: Set(row.tool_call_count),
        tool_error_count: Set(row.tool_error_count),
        input_tokens: Set(row.input_tokens),
        output_tokens: Set(row.output_tokens),
        reasoning_tokens: Set(row.reasoning_tokens),
        cache_creation_input_tokens: Set(row.cache_creation_input_tokens),
        cache_read_input_tokens: Set(row.cache_read_input_tokens),
        computed_total_tokens: Set(row.computed_total_tokens),
        retryable: Set(0_i64),
        cancelled_by_user: Set(i64::from(row.cancelled_by_user)),
        context_exceeded: Set(i64::from(row.context_exceeded)),
        error_type: Set(None::<String>),
        error_code: Set(None::<String>),
    })
    .on_conflict(
        OnConflict::columns([
            turn_usage::Column::TenantId,
            turn_usage::Column::SessionId,
            turn_usage::Column::TurnId,
        ])
        .value(
            turn_usage::Column::TraceId,
            Expr::expr(Func::coalesce([
                Expr::col((Alias::new("excluded"), turn_usage::Column::TraceId)),
                Expr::col((turn_usage::Entity, turn_usage::Column::TraceId)),
            ])),
        )
        .value(
            turn_usage::Column::UserMessageId,
            Expr::expr(Func::coalesce([
                Expr::col((Alias::new("excluded"), turn_usage::Column::UserMessageId)),
                Expr::col((turn_usage::Entity, turn_usage::Column::UserMessageId)),
            ])),
        )
        .value(
            turn_usage::Column::Status,
            Expr::col((Alias::new("excluded"), turn_usage::Column::Status)),
        )
        .value(
            turn_usage::Column::StartedAt,
            Expr::expr(Func::cust(Alias::new("least")).args([
                Expr::col((turn_usage::Entity, turn_usage::Column::StartedAt)),
                Expr::col((Alias::new("excluded"), turn_usage::Column::StartedAt)),
            ])),
        )
        .value(
            turn_usage::Column::FirstModelStartAt,
            Expr::expr(Func::coalesce([
                Expr::col((turn_usage::Entity, turn_usage::Column::FirstModelStartAt)),
                Expr::col((
                    Alias::new("excluded"),
                    turn_usage::Column::FirstModelStartAt,
                )),
            ])),
        )
        .value(
            turn_usage::Column::FirstTokenAt,
            Expr::expr(Func::coalesce([
                Expr::col((turn_usage::Entity, turn_usage::Column::FirstTokenAt)),
                Expr::col((Alias::new("excluded"), turn_usage::Column::FirstTokenAt)),
            ])),
        )
        .value(
            turn_usage::Column::CompletedAt,
            Expr::expr(Func::coalesce([
                Expr::col((Alias::new("excluded"), turn_usage::Column::CompletedAt)),
                Expr::col((turn_usage::Entity, turn_usage::Column::CompletedAt)),
            ])),
        )
        .value(
            turn_usage::Column::DurationMs,
            Expr::expr(Func::coalesce([
                Expr::col((Alias::new("excluded"), turn_usage::Column::DurationMs)),
                Expr::col((turn_usage::Entity, turn_usage::Column::DurationMs)),
            ])),
        )
        .value(
            turn_usage::Column::TimeToFirstTokenMs,
            Expr::expr(Func::coalesce([
                Expr::col((
                    Alias::new("excluded"),
                    turn_usage::Column::TimeToFirstTokenMs,
                )),
                Expr::col((turn_usage::Entity, turn_usage::Column::TimeToFirstTokenMs)),
            ])),
        )
        .value(
            turn_usage::Column::ModelRequestCount,
            Expr::col((
                Alias::new("excluded"),
                turn_usage::Column::ModelRequestCount,
            )),
        )
        .value(
            turn_usage::Column::ModelRetryCount,
            Expr::col((Alias::new("excluded"), turn_usage::Column::ModelRetryCount)),
        )
        .value(
            turn_usage::Column::ToolCallCount,
            Expr::col((Alias::new("excluded"), turn_usage::Column::ToolCallCount)),
        )
        .value(
            turn_usage::Column::ToolErrorCount,
            Expr::col((Alias::new("excluded"), turn_usage::Column::ToolErrorCount)),
        )
        .value(
            turn_usage::Column::InputTokens,
            Expr::col((Alias::new("excluded"), turn_usage::Column::InputTokens)),
        )
        .value(
            turn_usage::Column::OutputTokens,
            Expr::col((Alias::new("excluded"), turn_usage::Column::OutputTokens)),
        )
        .value(
            turn_usage::Column::ReasoningTokens,
            Expr::col((Alias::new("excluded"), turn_usage::Column::ReasoningTokens)),
        )
        .value(
            turn_usage::Column::CacheCreationInputTokens,
            Expr::col((
                Alias::new("excluded"),
                turn_usage::Column::CacheCreationInputTokens,
            )),
        )
        .value(
            turn_usage::Column::CacheReadInputTokens,
            Expr::col((
                Alias::new("excluded"),
                turn_usage::Column::CacheReadInputTokens,
            )),
        )
        .value(
            turn_usage::Column::ComputedTotalTokens,
            Expr::col((
                Alias::new("excluded"),
                turn_usage::Column::ComputedTotalTokens,
            )),
        )
        .value(
            turn_usage::Column::Retryable,
            Expr::col((Alias::new("excluded"), turn_usage::Column::Retryable)),
        )
        .value(
            turn_usage::Column::CancelledByUser,
            Expr::col((Alias::new("excluded"), turn_usage::Column::CancelledByUser)),
        )
        .value(
            turn_usage::Column::ContextExceeded,
            Expr::col((Alias::new("excluded"), turn_usage::Column::ContextExceeded)),
        )
        .value(
            turn_usage::Column::ErrorType,
            Expr::expr(Func::coalesce([
                Expr::col((Alias::new("excluded"), turn_usage::Column::ErrorType)),
                Expr::col((turn_usage::Entity, turn_usage::Column::ErrorType)),
            ])),
        )
        .value(
            turn_usage::Column::ErrorCode,
            Expr::expr(Func::coalesce([
                Expr::col((Alias::new("excluded"), turn_usage::Column::ErrorCode)),
                Expr::col((turn_usage::Entity, turn_usage::Column::ErrorCode)),
            ])),
        )
        .to_owned(),
    )
    .exec(tx)
    .await?;
    Ok(())
}

/// Node `pruneUsage` 的三条 delete（Node 自己再开一个 `begin immediate`；这里已在提交的事务里）。
pub(crate) async fn prune(store: &PgSessionStore, tx: &Tx, before: i64) -> Result<(), DbErr> {
    store
        .delete_rows::<model_usage::Entity>()
        .filter(model_usage::Column::StartedAt.lt(before))
        .exec(tx)
        .await?;
    store
        .delete_rows::<turn_usage::Entity>()
        .filter(turn_usage::Column::StartedAt.lt(before))
        .exec(tx)
        .await?;
    store
        .delete_rows::<tool_usage::Entity>()
        .filter(tool_usage::Column::StartedAt.lt(before))
        .exec(tx)
        .await?;
    Ok(())
}

/// 一轮已有的用量行的聚合（同 node-store：模型请求数主循环的三种来源与压缩摘要的，usage.md §2.1.2）。
pub(crate) async fn turn_totals(
    store: &PgSessionStore,
    tx: &Tx,
    session: &str,
    turn: &str,
) -> Result<TurnTotals, DbErr> {
    let row = crate::orm::native(
        "select count(*)::bigint as requests, coalesce(sum(retry_count), 0)::bigint as retries,
           coalesce(sum(input_tokens), 0)::bigint as input, coalesce(sum(output_tokens), 0)::bigint as output,
           coalesce(sum(reasoning_tokens), 0)::bigint as reasoning,
           coalesce(sum(cache_creation_input_tokens), 0)::bigint as cache_write,
           coalesce(sum(cache_read_input_tokens), 0)::bigint as cache_read,
           coalesce(sum(computed_total_tokens), 0)::bigint as total, min(started_at) as first_start,
           min(first_token_at) as first_token, coalesce(max(context_exceeded), 0)::bigint as exceeded
         from zcode.model_usage where tenant_id = $1 and session_id = $2 and turn_id = $3
          and query_source in ('main_turn', 'subagent', 'workflow_child', 'compact')",
    )
    .bind(store.tenant())
    .bind(session)
    .bind(turn)
    .fetch_one(tx)
    .await?;
    use sqlx::Row;
    let mut totals = TurnTotals {
        requests: row.get("requests"),
        retries: row.get("retries"),
        input: row.get("input"),
        output: row.get("output"),
        reasoning: row.get("reasoning"),
        cache_write: row.get("cache_write"),
        cache_read: row.get("cache_read"),
        total: row.get("total"),
        first_start: row.get("first_start"),
        first_token: row.get("first_token"),
        context_exceeded: row.get::<i64, _>("exceeded") != 0,
        ..TurnTotals::default()
    };
    let tools = crate::orm::native(
        "select count(*)::bigint as tools,
           coalesce(sum(case when status = 'error' then 1 else 0 end), 0)::bigint as errors
         from zcode.tool_usage where tenant_id = $1 and session_id = $2 and turn_id = $3",
    )
    .bind(store.tenant())
    .bind(session)
    .bind(turn)
    .fetch_one(tx)
    .await?;
    (totals.tools, totals.tool_errors) = (tools.get("tools"), tools.get("errors"));
    Ok(totals)
}

pub(crate) async fn turn_started(
    store: &PgSessionStore,
    tx: &Tx,
    session: &str,
    turn: &str,
) -> Result<Option<i64>, DbErr> {
    Ok(store
        .find::<turn_usage::Entity>()
        .filter(turn_usage::Column::SessionId.eq(session))
        .filter(turn_usage::Column::TurnId.eq(turn))
        .one(tx)
        .await?
        .map(|row| row.started_at))
}

pub(crate) async fn turn_failed(
    store: &PgSessionStore,
    tx: &Tx,
    session: &str,
    user_message: &str,
) -> Result<bool, DbErr> {
    let status = store
        .find::<turn_usage::Entity>()
        .filter(turn_usage::Column::SessionId.eq(session))
        .filter(turn_usage::Column::UserMessageId.eq(user_message))
        .order_by_desc(turn_usage::Column::CompletedAt)
        .one(tx)
        .await?
        .map(|row| row.status);
    Ok(status.as_deref() == Some("error"))
}
