//! 旧 Node 表的 ORM 映射；JSON 继续以原始文本保存，沿用现有编解码规则。
pub(crate) mod session {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "session")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub project_id: String,
        pub workspace_id: Option<String>,
        pub parent_id: Option<String>,
        pub slug: String,
        pub directory: String,
        pub path: Option<String>,
        pub title: String,
        pub version: String,
        pub share_url: Option<String>,
        pub summary_additions: Option<i64>,
        pub summary_deletions: Option<i64>,
        pub summary_files: Option<i64>,
        pub summary_diffs: Option<String>,
        pub revert: Option<String>,
        pub permission: Option<String>,
        pub time_created: i64,
        pub time_updated: i64,
        pub time_compacting: Option<i64>,
        pub time_archived: Option<i64>,
        pub task_type: String,
        pub title_source: String,
        pub title_message_id: Option<String>,
        pub time_title_updated: Option<i64>,
        pub trace_id: Option<String>,
        pub time_deleted: Option<i64>,
        pub last_outcome: Option<String>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod message {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "message")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub session_id: String,
        pub time_created: i64,
        pub time_updated: i64,
        pub data: String,
        pub sequence: Option<i64>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod part {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "part")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub message_id: String,
        pub session_id: String,
        pub time_created: i64,
        pub time_updated: i64,
        pub data: String,
        pub sequence: Option<i64>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod session_entry {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "session_entry")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub session_id: String,
        #[sea_orm(column_name = "type")]
        pub kind: String,
        pub time_created: i64,
        pub time_updated: i64,
        pub data: String,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod local_setting {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "local_setting")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub scope: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub scope_id: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub namespace: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        pub value: String,
        pub schema_version: i64,
        pub time_created: i64,
        pub time_updated: i64,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod model_usage {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "model_usage")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
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
        pub status: String,
        pub started_at: i64,
        pub first_token_at: Option<i64>,
        pub completed_at: Option<i64>,
        pub duration_ms: Option<i64>,
        pub time_to_first_token_ms: Option<i64>,
        pub finish_reason: Option<String>,
        pub tool_call_count: i64,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub reasoning_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub provider_total_tokens: Option<i64>,
        pub computed_total_tokens: i64,
        pub retry_count: i64,
        pub retryable: i64,
        pub cancelled_by_user: i64,
        pub context_exceeded: i64,
        pub error_type: Option<String>,
        pub error_code: Option<String>,
        pub error_message: Option<String>,
        pub raw_usage_json: Option<String>,
        pub provider_metadata_json: Option<String>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod turn_usage {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_usage")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub session_id: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub turn_id: String,
        pub trace_id: Option<String>,
        pub user_message_id: Option<String>,
        pub status: String,
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
        pub retryable: i64,
        pub cancelled_by_user: i64,
        pub context_exceeded: i64,
        pub error_type: Option<String>,
        pub error_code: Option<String>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod tool_usage {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "tool_usage")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub session_id: String,
        pub turn_id: Option<String>,
        pub trace_id: Option<String>,
        pub tool_call_id: String,
        pub tool_name: String,
        pub side_effect_scope: Option<String>,
        pub read_only: Option<i64>,
        pub destructive: Option<i64>,
        pub approval_status: Option<String>,
        pub status: String,
        pub started_at: i64,
        pub first_output_at: Option<i64>,
        pub completed_at: Option<i64>,
        pub duration_ms: Option<i64>,
        pub time_to_first_output_ms: Option<i64>,
        pub exit_code: Option<i64>,
        pub output_bytes: i64,
        pub stdout_bytes: i64,
        pub stderr_bytes: i64,
        pub truncated: i64,
        pub retry_count: i64,
        pub retryable: i64,
        pub cancelled_by_user: i64,
        pub error_type: Option<String>,
        pub error_code: Option<String>,
        pub error_message: Option<String>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod permission {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "permission")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub project_id: String,
        pub time_created: i64,
        pub time_updated: i64,
        pub data: String,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod todo {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "todo")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub session_id: String,
        pub content: String,
        pub status: String,
        pub priority: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub position: i64,
        pub time_created: i64,
        pub time_updated: i64,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
pub(crate) mod session_target {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "session_target")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub session_id: String,
        pub target_id: String,
        pub objective: String,
        pub status: String,
        pub token_budget: Option<i64>,
        pub tokens_used: i64,
        pub time_used_seconds: i64,
        pub time_created: i64,
        pub time_updated: i64,
        pub summary_title: Option<String>,
        pub active_input_id: Option<String>,
        pub active_run_started_at: Option<i64>,
        pub active_run_last_seen_at: Option<i64>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
