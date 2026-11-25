//! 用量的只读聚合（docs/specs/rust-cli/usage.md §4、§5.2）与有界清理（pg-store.md §6 第 5 步）。查询照 node-store 的
//! `usage_query.rs` / `usage_stats.rs`，读同一组 Node 表；另加租户与项目条件，JSON 判断换成 PG 的写法。写入在
//! node-records 的编排里（经 [`crate::rows::PgRows`]）。
use rust_cli_kernel_api::clock::civil_date;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::metering::{
    ActivityDay, ActivityQuery, ActivityRange, ActivityStats, LastRequest, ModelTokens,
    RequestStats, SessionMetering,
};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_node_records::usage::RETENTION_MS;
use sqlx::Row;

use crate::orm::error as orm_error;
use crate::{PgSessionStore, number, storage};

const DAY_MS: i64 = rust_cli_base::js::date::DAY_MS as i64;

/// 会话及其子代理子会话里成功请求的耗时之和（`Total duration (API)`）。
const METERING: &str = "with recursive tree(id) as (
       select $2::text
       union
       select s.id from zcode.session s join tree t on s.parent_id = t.id
        where s.tenant_id = $1 and s.task_type = 'subagent_child'
     )
     select coalesce(sum(duration_ms), 0)::bigint as total from zcode.model_usage
      where tenant_id = $1 and session_id in (select id from tree) and status = 'completed'
        and started_at >= $3";

/// 统计里的会话：未删除、不是子代理或工作流的子运行，且属于绑定的项目。
const COUNTED: &str = "s.tenant_id = $1 and s.project_id = any($2) and s.time_deleted is null
       and s.task_type not in ('subagent_child', 'workflow_child', 'nested_workflow_child')";

fn sessions_sql() -> String {
    format!(
        "with started as (
           select (select min(m.time_created) from zcode.message m
                    where m.tenant_id = s.tenant_id and m.session_id = s.id) as first,
                  (select max(m.time_created) from zcode.message m
                    where m.tenant_id = s.tenant_id and m.session_id = s.id) as last
             from zcode.session s where {COUNTED}
         )
         select count(*)::bigint as sessions, max(last - first) as longest, min(first) as first,
                max(first) as latest
           from started where first is not null and first >= $3"
    )
}

fn days_sql() -> String {
    format!(
        "select floor((m.time_created + $3)::numeric / {DAY_MS})::bigint as day, count(*)::bigint as count
           from zcode.session s join zcode.message m on m.tenant_id = s.tenant_id and m.session_id = s.id
          where {COUNTED} and m.time_created >= $4
          group by day order by day"
    )
}

const MODELS: &str = "select model_id,
       coalesce(sum(case
         when input_tokens <= 0 then 0
         when cache_read_input_tokens + cache_creation_input_tokens <= 0 then input_tokens
         when coalesce(provider_total_tokens, computed_total_tokens) > 0
          and abs(coalesce(provider_total_tokens, computed_total_tokens)
                  - (input_tokens + cache_read_input_tokens + cache_creation_input_tokens + output_tokens))
            < abs(coalesce(provider_total_tokens, computed_total_tokens) - (input_tokens + output_tokens))
           then input_tokens
         else greatest(input_tokens - cache_read_input_tokens - cache_creation_input_tokens, 0)
       end), 0)::bigint as input,
       coalesce(sum(output_tokens), 0)::bigint as output,
       coalesce(sum(cache_read_input_tokens), 0)::bigint as cached,
       coalesce(sum(cache_creation_input_tokens), 0)::bigint as written
     from zcode.model_usage u
    where u.tenant_id = $1
      and u.session_id in (select id from zcode.session where tenant_id = $1 and project_id = any($2))
      and query_source in ('main_turn', 'subagent', 'workflow_child') and started_at >= $3
    group by model_id";

/// `raw_usage_json` 里这一项是非负数（node-store `usage_stats::known`）。
/// 量得到速度的一行：输出已知且大于 0、有首字时刻、总时长大于首字时刻（同 node-store 的 `measured`）。
fn measured() -> String {
    format!(
        "{output} = 1 and output_tokens > 0 and time_to_first_token_ms is not null
         and duration_ms > time_to_first_token_ms",
        output = known("outputTokens"),
    )
}

fn known(key: &str) -> String {
    format!(
        "(case when raw_usage_json is null or raw_usage_json is not json object then 0
               when jsonb_typeof(raw_usage_json::jsonb -> '{key}') = 'number'
                and (raw_usage_json::jsonb ->> '{key}')::numeric >= 0 then 1
               else 0 end)"
    )
}

/// 用量只统计保留期（30 天）之内的（§4）：旧行在维护命令清理之前也不计入。来源与 node-store 的 `usage_stats`
/// 相同（修复线 F112）：本会话自己的主循环，主会话是 `main_turn`，子会话是 `subagent`。
const REQUESTS: &str =
    "tenant_id = $1 and session_id = $2 and query_source in ('main_turn', 'subagent')
       and status = 'completed' and started_at >= $3";

const INPUT_SIDE: &str = "case
       when input_tokens <= 0 then cache_read_input_tokens + cache_creation_input_tokens
       when cache_read_input_tokens + cache_creation_input_tokens <= 0 then input_tokens
       when coalesce(provider_total_tokens, computed_total_tokens) > 0
        and abs(coalesce(provider_total_tokens, computed_total_tokens)
                - (input_tokens + cache_read_input_tokens + cache_creation_input_tokens + output_tokens))
          < abs(coalesce(provider_total_tokens, computed_total_tokens) - (input_tokens + output_tokens))
         then input_tokens + cache_read_input_tokens + cache_creation_input_tokens
       else input_tokens
     end";

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl PgSessionStore {
    fn usage_since(&self) -> Result<i64, StoreError> {
        Ok(number(self.clock.now_ms())?.saturating_sub(RETENTION_MS))
    }

    pub(crate) async fn session_metering(
        &self,
        session: &SessionId,
    ) -> Result<SessionMetering, StoreError> {
        let mut tx = self.begin(true).await?;
        self.live(&mut tx, session, false).await?;
        let row = crate::orm::native(METERING)
            .bind(self.tenant())
            .bind(session.as_str())
            .bind(self.usage_since()?)
            .fetch_one(&tx)
            .await
            .map_err(orm_error)?;
        tx.commit().await.map_err(orm_error)?;
        Ok(SessionMetering {
            api_duration_ms: count(row.get("total")),
        })
    }

    pub(crate) async fn stats(&self, session: &SessionId) -> Result<RequestStats, StoreError> {
        let mut tx = self.begin(true).await?;
        self.live(&mut tx, session, false).await?;
        // 已知的输出之和与“量得到速度”的输出与生成时长之和（子代理的 token 列与平均速度），算法同 node-store 的
        // `request_totals`（usage_reports.rs 的 `totals_sql`、`measured`）。
        let totals = crate::orm::native(&format!(
            "select count(*)::bigint as requests, coalesce(sum({INPUT_SIDE}), 0)::bigint as input,
                    coalesce(sum(cache_read_input_tokens), 0)::bigint as cached,
                    coalesce(sum(1 - {input} * {read}), 0)::bigint as unknown,
                    coalesce(sum(case when {output} = 1 then output_tokens else 0 end), 0)::bigint as output,
                    coalesce(sum(case when {measured} then output_tokens else 0 end), 0)::bigint as measured_output,
                    coalesce(sum(case when {measured} then duration_ms - time_to_first_token_ms else 0 end), 0)::bigint
                      as generation_ms
               from zcode.model_usage where {REQUESTS}",
            input = known("inputTokens"),
            read = known("cacheReadTokens"),
            output = known("outputTokens"),
            measured = measured(),
        ))
        .bind(self.tenant())
        .bind(session.as_str())
        .bind(self.usage_since()?)
        .fetch_one(&tx)
        .await
        .map_err(orm_error)?;
        let last = crate::orm::native(&format!(
            "select case when {output} = 1 then output_tokens end as output, duration_ms,
                    time_to_first_token_ms
               from zcode.model_usage where {REQUESTS}
              order by completed_at desc nulls last, started_at desc limit 1",
            output = known("outputTokens"),
        ))
        .bind(self.tenant())
        .bind(session.as_str())
        .bind(self.usage_since()?)
        .fetch_optional(&tx)
        .await
        .map_err(orm_error)?;
        tx.commit().await.map_err(orm_error)?;
        Ok(RequestStats {
            requests: count(totals.get("requests")),
            input_tokens: count(totals.get("input")),
            cache_read_tokens: count(totals.get("cached")),
            unknown_cache_usage: totals.get::<i64, _>("unknown") > 0,
            last: last.map(|row| LastRequest {
                output_tokens: row.get::<Option<i64>, _>("output").map(count),
                duration_ms: row.get::<Option<i64>, _>("duration_ms").map(count),
                first_token_ms: row
                    .get::<Option<i64>, _>("time_to_first_token_ms")
                    .map(count),
            }),
            output_tokens: count(totals.get("output")),
            measured_output_tokens: count(totals.get("measured_output")),
            generation_ms: count(totals.get("generation_ms")),
        })
    }

    pub(crate) async fn activity_stats(
        &self,
        query: ActivityQuery,
    ) -> Result<ActivityStats, StoreError> {
        let offset =
            i64::from(query.utc_offset_minutes) * rust_cli_base::units::MS_PER_MINUTE as i64;
        let today = rust_cli_base::civil::parse_date(&query.today)
            .ok_or_else(|| storage(format!("not a date: {}", query.today)))?;
        let from = match query.range {
            ActivityRange::All => i64::MIN,
            range => {
                let days = i64::try_from(range.days().unwrap_or(0)).unwrap_or(0);
                (today - (days - 1)) * DAY_MS - offset
            }
        };
        let projects = self.project_ids();
        let tx = self.begin(true).await?;
        let started = crate::orm::native(&sessions_sql())
            .bind(self.tenant())
            .bind(projects.clone())
            .bind(from)
            .fetch_one(&tx)
            .await
            .map_err(orm_error)?;
        let days = crate::orm::native(&days_sql())
            .bind(self.tenant())
            .bind(projects.clone())
            .bind(offset)
            .bind(from)
            .fetch_all(&tx)
            .await
            .map_err(orm_error)?
            .into_iter()
            .map(|row| ActivityDay {
                date: civil_date(count(row.get("day")) * rust_cli_base::js::date::DAY_MS),
                messages: count(row.get("count")),
            })
            .collect();
        let mut models: Vec<ModelTokens> = crate::orm::native(MODELS)
            .bind(self.tenant())
            .bind(projects)
            .bind(from.max(self.usage_since()?))
            .fetch_all(&tx)
            .await
            .map_err(orm_error)?
            .into_iter()
            .map(|row| ModelTokens {
                model: row.get("model_id"),
                input: count(row.get("input")),
                output: count(row.get("output")),
                cache_read: count(row.get("cached")),
                cache_write: count(row.get("written")),
            })
            .collect();
        tx.commit().await.map_err(orm_error)?;
        models.sort_by(|a, b| {
            b.total()
                .cmp(&a.total())
                .then_with(|| a.model.cmp(&b.model))
        });
        models.retain(|model| model.total() > 0);
        let optional = |key: &str| started.get::<Option<i64>, _>(key).map(count);
        Ok(ActivityStats {
            range: Some(query.range),
            today: query.today.clone(),
            days,
            models,
            sessions: count(started.get("sessions")),
            longest_session_ms: optional("longest"),
            first_session_ms: optional("first"),
            last_session_ms: optional("latest"),
        })
    }

    /// 维护命令：删除保留期（30 天）之前开始的用量行，每张表一次最多 `limit` 行，返回删除的行数。
    pub async fn prune_usage(&self, limit: u32) -> Result<u64, StoreError> {
        let before = number(self.clock.now_ms())?.saturating_sub(RETENTION_MS);
        let tx = self.begin(false).await?;
        let mut removed = 0;
        for table in ["model_usage", "tool_usage", "turn_usage"] {
            let key = if table == "turn_usage" {
                "tenant_id, session_id, turn_id"
            } else {
                "tenant_id, id"
            };
            let sql = format!(
                "delete from zcode.{table} where ({key}) in (select {key} from zcode.{table}
                   where tenant_id = $1 and started_at < $2 limit $3)"
            );
            removed += crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(before)
                .bind(i64::from(limit))
                .execute(&tx)
                .await
                .map_err(orm_error)?
                .rows_affected();
        }
        tx.commit().await.map_err(orm_error)?;
        Ok(removed)
    }
}
