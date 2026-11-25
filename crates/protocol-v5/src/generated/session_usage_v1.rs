//! 排版之前的摘要：7def97e91cad2d28
//! 生成物，不要手改：`session.usage@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionUsageParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionUsageResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "reasoningTokens")]
    pub reasoning_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "modelRequestCount")]
    pub model_request_count: u64,
    #[serde(rename = "modelErrorCount")]
    pub model_error_count: u64,
    #[serde(
        rename = "inputBaselineBySource",
        default = "default_query_session_usage_result_input_baseline_by_source"
    )]
    pub input_baseline_by_source: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsParams {
    #[serde(default = "default_query_usage_stats_params_range")]
    pub range: QueryUsageStatsParamsRange,
    #[serde(rename = "timeZone", default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueryUsageStatsParamsRange {
    All,
    V7d,
    V30d,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QueryUsageStatsParamsRange {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::V30d;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::All => "all",
            Self::V7d => "7d",
            Self::V30d => "30d",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "all" => Self::All,
            "7d" => Self::V7d,
            "30d" => Self::V30d,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::V30d,
            known => known.clone(),
        }
    }
}

impl Serialize for QueryUsageStatsParamsRange {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueryUsageStatsParamsRange {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResult {
    #[serde(default = "fallback_query_usage_stats_result_range")]
    pub range: QueryUsageStatsParamsRange,
    #[serde(rename = "generatedAt")]
    pub generated_at: f64,
    #[serde(rename = "timeZone")]
    pub time_zone: String,
    pub source: QueryUsageStatsResultSource,
    pub summary: QueryUsageStatsResultSummary,
    pub heatmap: QueryUsageStatsResultHeatmap,
    #[serde(rename = "dailyModelUsage")]
    pub daily_model_usage: Vec<QueryUsageStatsResultDailyModelUsageItem>,
    pub models: Vec<QueryUsageStatsResultModelsItem>,
    pub tools: Vec<QueryUsageStatsResultToolsItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryUsageStatsResultSource {
    #[serde(rename = "agent-db")]
    AgentDb,
}

impl QueryUsageStatsResultSource {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentDb => "agent-db",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultSummary {
    #[serde(rename = "totalTokens")]
    pub total_tokens: f64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: f64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: f64,
    #[serde(rename = "reasoningTokens")]
    pub reasoning_tokens: f64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: f64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: f64,
    #[serde(rename = "cacheHitRate")]
    pub cache_hit_rate: f64,
    #[serde(rename = "totalSessions")]
    pub total_sessions: f64,
    #[serde(rename = "totalTurns")]
    pub total_turns: f64,
    #[serde(rename = "toolCallCount")]
    pub tool_call_count: f64,
    #[serde(rename = "toolErrorRate")]
    pub tool_error_rate: f64,
    #[serde(rename = "modelErrorRate")]
    pub model_error_rate: f64,
    #[serde(rename = "avgTimeToFirstTokenMs")]
    pub avg_time_to_first_token_ms: Option<f64>,
    #[serde(rename = "avgTurnDurationMs")]
    pub avg_turn_duration_ms: Option<f64>,
    #[serde(rename = "activeDays")]
    pub active_days: f64,
    #[serde(rename = "currentStreakDays")]
    pub current_streak_days: f64,
    #[serde(rename = "longestSessionMs")]
    pub longest_session_ms: f64,
    #[serde(rename = "longestStreakDays")]
    pub longest_streak_days: f64,
    #[serde(rename = "peakDayTokens")]
    pub peak_day_tokens: f64,
    #[serde(rename = "favoriteModel")]
    pub favorite_model: Option<QueryUsageStatsResultSummaryFavoriteModel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultSummaryFavoriteModel {
    #[serde(rename = "modelId")]
    pub model_id: Option<String>,
    #[serde(rename = "totalTokens")]
    pub total_tokens: f64,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultHeatmap {
    #[serde(rename = "startDate")]
    pub start_date: Option<String>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(rename = "maxTokens")]
    pub max_tokens: f64,
    pub weeks: Vec<QueryUsageStatsResultHeatmapWeeksItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultHeatmapWeeksItem {
    #[serde(rename = "weekIndex")]
    pub week_index: f64,
    pub days: Vec<Option<QueryUsageStatsResultHeatmapWeeksItemDaysItem>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultHeatmapWeeksItemDaysItem {
    pub date: String,
    pub level: QueryUsageStatsResultHeatmapWeeksItemDaysItemLevel,
    #[serde(rename = "totalTokens")]
    pub total_tokens: f64,
    #[serde(rename = "turnCount")]
    pub turn_count: f64,
    #[serde(rename = "toolCallCount")]
    pub tool_call_count: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueryUsageStatsResultHeatmapWeeksItemDaysItemLevel {
    Alt1(i64),
    Alt2(i64),
    Alt3(i64),
    Alt4(i64),
    Alt5(i64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultDailyModelUsageItem {
    pub date: String,
    pub models: Vec<QueryUsageStatsResultDailyModelUsageItemModelsItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultDailyModelUsageItemModelsItem {
    #[serde(rename = "modelId")]
    pub model_id: Option<String>,
    #[serde(rename = "totalTokens")]
    pub total_tokens: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultModelsItem {
    #[serde(rename = "modelId")]
    pub model_id: Option<String>,
    #[serde(rename = "totalTokens")]
    pub total_tokens: f64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: f64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: f64,
    #[serde(rename = "requestCount")]
    pub request_count: f64,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryUsageStatsResultToolsItem {
    #[serde(rename = "toolName")]
    pub tool_name: String,
    #[serde(rename = "callCount")]
    pub call_count: f64,
    #[serde(rename = "errorCount")]
    pub error_count: f64,
    #[serde(rename = "errorRate")]
    pub error_rate: f64,
    #[serde(rename = "avgDurationMs")]
    pub avg_duration_ms: Option<f64>,
}

fn default_query_session_usage_result_input_baseline_by_source() -> BTreeMap<String, u64> {
    BTreeMap::new()
}

fn default_query_usage_stats_params_range() -> QueryUsageStatsParamsRange {
    QueryUsageStatsParamsRange::V30d
}

fn fallback_query_usage_stats_result_range() -> QueryUsageStatsParamsRange {
    QueryUsageStatsParamsRange::V30d
}
