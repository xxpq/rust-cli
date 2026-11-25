//! Node 会话库的行操作（docs/specs/rust-cli/pg-store.md §12）：写入与冷读取的编排只经这个接口读写行。本机的实现是
//! node-store 的 rusqlite 仓储，云端的实现是 pg-store 的 PostgreSQL。两边在同一个事务里实现全部方法，
//! 编排因此写出相同的 Node 记录。行的形状与列名都是 Node 的（`repositories/*.ts`）。
use rust_cli_kernel_api::store::StoreError;
use serde_json::Value;

use crate::row::WithParts;

/// Node `session` 行里编排用到的列。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub trace_id: Option<String>,
    pub task_type: String,
    pub directory: String,
    pub title: String,
    /// `title_source`（NULL 是 Node 的缺省 `first_input`）。
    pub title_source: Option<String>,
    pub revert: Option<Value>,
    pub permission: Option<Value>,
    /// Node `lastOutcome`：completed / interrupted / error。
    pub last_outcome: Option<String>,
    pub time_created: i64,
    pub time_updated: i64,
    pub time_deleted: Option<i64>,
}

/// App 任务列表的会话种类（Node `TASK_LIST_SESSION_TYPES`，sessions-index 的冷种子与 `query_sessions` 的口径）。
pub const LISTED_TASK_TYPES: [&str; 3] = ["interactive", "fork", "workflow_parent"];

impl SessionRow {
    /// 列表会话：本机、种类在 [`LISTED_TASK_TYPES`] 里（会话列表的事实只给它们，session-facts.md §3）。
    pub fn listed(&self) -> bool {
        self.workspace_id.is_none() && LISTED_TASK_TYPES.contains(&self.task_type.as_str())
    }
}

/// Node `createSession` 的输入。
#[derive(Clone, Debug, Default)]
pub struct Create {
    pub id: String,
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub trace_id: Option<String>,
    pub task_type: Option<String>,
    pub slug: String,
    pub directory: String,
    pub path: Option<String>,
    pub title: String,
    pub title_source: Option<String>,
    pub version: String,
    pub permission: Option<Value>,
    pub time_created: Option<i64>,
}

/// Node `updateSession` 的输入：`None` 的列不改；`expected_title_sources` 非空时只在当前来源在其中时改标题（CAS）。
#[derive(Clone, Debug, Default)]
pub struct Update {
    pub id: String,
    pub title: Option<String>,
    pub expected_title_sources: Vec<String>,
    pub title_source: Option<String>,
    pub title_message_id: Option<String>,
    pub revert: Option<Value>,
    pub permission: Option<Value>,
    pub last_outcome: Option<String>,
    pub time_updated: Option<i64>,
}

/// Node `SessionEntryInfo`。
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub time_created: i64,
    pub time_updated: i64,
    /// 逻辑数据；模型选择写入时包成 `{modelSelection}`，读取时拆开。
    pub data: Value,
    /// Node `touchSession`；默认推进会话的活动时间。
    pub touch_session: bool,
}

/// `model_usage` 的一行（列按 Node 插入的顺序）。
#[derive(Debug, Clone, Default)]
pub struct ModelRow {
    pub id: String,
    pub logical_request_id: String,
    pub attempt_index: i64,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub parent_user_message_id: Option<String>,
    pub query_source: String,
    pub provider_id: String,
    pub model_id: String,
    pub variant: Option<String>,
    pub agent: Option<String>,
    pub mode: Option<String>,
    pub task_type: Option<String>,
    pub status: &'static str,
    pub started_at: i64,
    pub first_token_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub time_to_first_token_ms: Option<i64>,
    pub finish_reason: Option<&'static str>,
    pub tool_call_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub provider_total_tokens: Option<i64>,
    pub computed_total_tokens: i64,
    pub retry_count: i64,
    pub retryable: bool,
    pub cancelled_by_user: bool,
    pub context_exceeded: bool,
    pub error_type: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub raw_usage_json: Option<String>,
    pub provider_metadata_json: Option<String>,
}

/// `tool_usage` 的一行（Node `toolUsageValues` 的顺序）。
#[derive(Debug, Clone, Default)]
pub struct ToolRow {
    pub id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub trace_id: Option<String>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub approval_status: Option<&'static str>,
    pub status: &'static str,
    pub started_at: i64,
    pub first_output_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub time_to_first_output_ms: Option<i64>,
    pub output_bytes: i64,
    pub cancelled_by_user: bool,
}

/// `turn_usage` 的一行（Node 插入的顺序）。
#[derive(Debug, Clone, Default)]
pub struct TurnRow {
    pub session_id: String,
    pub turn_id: String,
    pub trace_id: Option<String>,
    pub user_message_id: Option<String>,
    pub status: &'static str,
    pub started_at: i64,
    pub first_model_start_at: Option<i64>,
    pub first_token_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub time_to_first_token_ms: Option<i64>,
    pub model_request_count: i64,
    pub model_retry_count: i64,
    pub tool_call_count: i64,
    pub tool_error_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub computed_total_tokens: i64,
    pub cancelled_by_user: bool,
    pub context_exceeded: bool,
}

/// 一轮已有的用量行的聚合（`turn_usage` 的计数与 token，usage.md §2.4）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnTotals {
    pub requests: i64,
    pub retries: i64,
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cache_write: i64,
    pub cache_read: i64,
    pub total: i64,
    pub first_start: Option<i64>,
    pub first_token: Option<i64>,
    pub context_exceeded: bool,
    pub tools: i64,
    pub tool_errors: i64,
}

/// 一个 tool part 行：(part id, 消息 id, data 文本)。
pub type PartRow = (String, String, String);

/// fork 新会话要从原会话行取的列（Node `buildForkedSessionInput`）；JSON 列是原文。
#[derive(Clone, Debug, Default)]
pub struct ForkSource {
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub trace_id: Option<String>,
    pub slug: String,
    pub directory: String,
    pub path: Option<String>,
    pub title: String,
    pub version: String,
    pub permission: Option<String>,
    pub revert: Option<String>,
}

/// `session_target` 行（Node `copyGoalStateForFork` 读写的列）。
#[derive(Clone, Debug, Default)]
pub struct TargetRow {
    pub session_id: String,
    pub target_id: String,
    pub objective: String,
    pub summary_title: Option<String>,
    pub status: String,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub time_created: i64,
    pub time_updated: i64,
}

/// 行操作的结果：错误已经是分好类的存储错误（云端的断线是 `Unavailable`，Actor 据此暂停写入；其余是 `Storage`）。
/// 以前是 `anyhow::Result`，编排要把它换回 `StoreError`，分好的类别只能靠 `Any` 向下转型找回来（`OrStorage`）。
pub type Result<T> = std::result::Result<T, StoreError>;

/// 编排用到的全部行操作；所有方法都在调用方的同一个事务里执行，实现给出分好类的存储错误。
pub trait Rows {
    // session 行（Node `repositories/sessions.ts`）。
    fn session(&self, id: &str) -> Result<Option<SessionRow>>;
    /// 未删除的会话行的 fork 来源列。
    fn fork_source(&self, id: &str) -> Result<Option<ForkSource>>;
    fn session_project_id(&self, id: &str) -> Result<Option<String>>;
    fn create_session(&self, input: &Create, now: i64) -> Result<()>;
    /// 改了的行数（标题 CAS 不满足时为 0）。
    fn update_session(&self, input: &Update, now: i64) -> Result<usize>;
    fn touch_session(&self, id: &str, time_updated: i64) -> Result<()>;
    fn soft_delete_session(&self, id: &str, time_deleted: i64) -> Result<()>;
    /// 本目录的顶层会话与 fork，按活动时间倒序。
    fn list_sessions(&self, directory: &str, limit: Option<i64>) -> Result<Vec<SessionRow>>;
    /// 会话分页查询（按创建或活动时间倒序）。
    fn query_sessions(
        &self,
        directory: Option<&str>,
        created: bool,
        limit: Option<i64>,
    ) -> Result<Vec<SessionRow>>;
    /// 会话首条真实输入的第一个非合成 text part 的 data 文本。
    fn first_prompt_part(&self, session: &str) -> Result<Option<String>>;
    /// 这些 id 的会话行（含软删除的；顺序不定，调用方按自己的顺序取）。
    fn sessions_by_ids(&self, ids: &[&str]) -> Result<Vec<SessionRow>>;
    /// 首条真实用户输入（`role = user`、`semantics.origin = real_user`）的创建时间。
    fn first_input_at(&self, session: &str) -> Result<Option<i64>>;
    /// 会话的第一条消息（按 `time_created, id`）的 id 与 data 文本。
    fn first_message(&self, session: &str) -> Result<Option<(String, String)>>;
    /// 这条消息的 part 里有没有 `metadata.<key> = value` 的。
    fn part_metadata_is(&self, message: &str, key: &str, value: &str) -> Result<bool>;

    // message / part 行（Node `repositories/messages.ts`）。
    fn save_message(&self, info: &Value, now: i64) -> Result<()>;
    fn save_part(&self, part: &Value, now: i64) -> Result<()>;
    /// 会话的全部消息，按 Node 的行序，part 按消息分组；part 只留 data 原文（冷读取逐条解码，node-store.md §5.4）。
    fn messages_raw(&self, session: &str) -> Result<Vec<crate::raw::RawRecord>>;
    /// 会话里给出的这些消息（按 id）与它们的 part，消息的行序与 part 的组同 [`Rows::messages_raw`]；库里没有的不给出（前端分页按 id 读历史的一段，
    /// node-store.md §5.5）。
    fn messages_raw_of(&self, session: &str, ids: &[&str]) -> Result<Vec<crate::raw::RawRecord>>;
    /// 会话的全部消息，整份解码（[`Rows::messages_raw`] 逐条解码）。
    fn messages(&self, session: &str) -> Result<Vec<WithParts>> {
        self.messages_raw(session)?
            .iter()
            .map(|record| record.decode().map_err(crate::storage))
            .collect()
    }
    /// 会话的全部消息的 id，按 [`Rows::messages_raw`] 的消息行序（子代理的冷读只按 id 选分支，subagents.rs）。
    fn message_ids(&self, session: &str) -> Result<Vec<String>>;
    /// 会话的全部消息行的头（id、role、summary，[`crate::raw::MessageHead::parse`]），按 [`Rows::messages_raw`] 的消息行序；不读 part、不建整份 info
    /// （范围读的历史索引，node-store.md §5.6）。
    fn message_heads(&self, session: &str) -> Result<Vec<crate::raw::MessageHead>>;
    /// 会话里 `type = tool`、`tool` 在 `tools` 里的 part（原文），带它所在消息的 id；同一条消息里按 [`Rows::messages_raw`] 的 part 行序。
    /// data 不是合法 JSON 的不选（子代理的冷读只要 Agent 调用，不读整段历史，docs/specs/rust-cli/node-store.md §5.4）。
    fn tool_parts(
        &self,
        session: &str,
        tools: &[&str],
    ) -> Result<Vec<(String, crate::raw::RawPart)>>;
    fn message_exists(&self, id: &str) -> Result<bool>;
    /// 本会话里的一条消息的 data 文本。
    fn message_data(&self, session: &str, id: &str) -> Result<Option<String>>;
    /// 最后一条真实用户输入（`semantics.origin = real_user`）的 (id, data)。
    fn current_user_row(&self, session: &str) -> Result<Option<(String, String)>>;
    /// 会话里最后一条消息行的 id（按 `sequence`）。
    fn last_message_id(&self, session: &str) -> Result<Option<String>>;
    /// 某个调用最后写的 tool part；`message` 给出时只在这条 assistant 里找。
    fn tool_part(
        &self,
        session: &str,
        message: Option<&str>,
        call: &str,
    ) -> Result<Option<PartRow>>;
    /// 这个 assistant 里已经有结果的 tool part 数。
    fn settled_tool_parts(&self, session: &str, message: &str) -> Result<u64>;
    /// 工具结果消息（没有行）所在的 assistant：按 `metadata.rustCli.resultMessageId` 或 tool part 的 id 找。
    fn result_owner(&self, session: &str, id: &str) -> Result<Option<String>>;

    // session_entry 行（Node `repositories/session-entries.ts`）。
    fn save_entry(&self, entry: &Entry) -> Result<()>;
    /// 会话的 entry，按 Node 的行序；`kind` 给出时只取这一种。
    fn entries(&self, session: &str, kind: Option<&str>) -> Result<Vec<Entry>>;
    /// 一条 entry 的 data 文本（本会话里，按 id）。
    fn entry_data(&self, session: &str, id: &str) -> Result<Option<String>>;
    /// 这两种 entry 的 `data.sequenceNumber` 的最大值。
    fn max_sequence_number(&self, session: &str, kinds: [&str; 2]) -> Result<Option<i64>>;

    // local_setting 行与旧 permission 表。
    fn read_setting(
        &self,
        scope: &str,
        id: &str,
        namespace: &str,
        key: &str,
    ) -> Result<Option<String>>;
    /// `(scope, scope_id, namespace, key)` 的 upsert。
    fn write_setting(&self, key: (&str, &str, &str, &str), value: String, now: i64) -> Result<()>;
    /// `scope_id` 在 `ids` 之中的行：(scope_id, value)。
    fn settings_in(
        &self,
        scope: &str,
        namespace: &str,
        key: &str,
        ids: &[&str],
    ) -> Result<Vec<(String, String)>>;
    /// 旧 `permission` 表里这个项目的 data。
    fn legacy_permission(&self, project: &str) -> Result<Option<String>>;

    // session_target 行。
    fn session_target(&self, session: &str) -> Result<Option<TargetRow>>;
    fn insert_session_target(&self, row: &TargetRow) -> Result<()>;

    // 用量行（Node `repositories/usage.ts`）。
    fn record_model(&self, row: &ModelRow) -> Result<()>;
    fn upsert_tool(&self, row: &ToolRow) -> Result<()>;
    fn upsert_turn(&self, row: &TurnRow) -> Result<()>;
    fn prune_usage(&self, before: i64) -> Result<()>;
    fn turn_totals(&self, session: &str, turn: &str) -> Result<TurnTotals>;
    fn turn_started(&self, session: &str, turn: &str) -> Result<Option<i64>>;
    fn turn_failed(&self, session: &str, user_message: &str) -> Result<bool>;

    /// 用量的 savepoint（usage.md §2.5）：失败的写入只回退它自己。
    fn savepoint(&self) -> Result<()>;
    fn release_savepoint(&self) -> Result<()>;
    /// 回到 savepoint 并释放；事务已被数据库整体回滚时失败。
    fn rollback_savepoint(&self) -> Result<()>;
}
