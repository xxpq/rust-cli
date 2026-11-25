//! 自动模式的分类器端口（docs/specs/rust-cli/auto-classifier.md §15.2）。
//!
//! 判决链（policy）在自动模式下把要询问的动作交给分类器；分类器的实现（classifier crate）只经模型端口发请求。两边只认这里的
//! 类型，互不依赖。工具给分类器看的内容是 [`crate::tool::Tool::classifier_input`]；这一步的对话摘录随调用上下文来
//! （[`crate::tool::ToolUseContext::transcript`]）。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::message::Message;
use crate::permission::auto_mode::AutoModeRules;
use crate::tool::{Tool, ToolUseContext};

/// 交给分类器的一个动作：工具、这次的输入与调用上下文（会话、代理、取消、这次运行的工具、摘录），加进提示的用户规则。
pub struct ClassifyRequest<'a> {
    pub tool: &'a dyn Tool,
    pub input: &'a Value,
    pub context: &'a ToolUseContext,
    pub rules: UserRules<'a>,
}

/// 用户的规则（docs/specs/rust-cli/auto-classifier.md §16.7）：判决链从它的规则快照给，分类器拼进提示。
#[derive(Debug, Clone, Copy)]
pub struct UserRules<'a> {
    /// 设置里的 `autoMode` 规则：换掉提示里四个槽的缺省。
    pub auto_mode: &'a AutoModeRules,
    /// 用户的 deny 规则（规则串，去重）：提示里列出，让分类器拦下换个工具绕过它们的动作。
    pub deny: &'a [String],
}

/// 分类器的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierVerdict {
    Allow,
    /// 拦下：`reason` 是给模型看的理由（规则名或分类器写的一句话）。
    Block {
        reason: String,
    },
    /// 没有结论：判决链按种类拒绝、放行或问人（auto-classifier.md §4.6）。
    NoVerdict(NoVerdict),
}

/// 没有结论的种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoVerdict {
    /// 工具的投影是空的：没有要判的内容。
    NothingToJudge,
    /// 回复解析不了（重发之后仍然）。
    Unparseable,
    /// 模型拒绝回答（安全拦截）。
    Refused,
    /// 请求失败。
    Unavailable(Unavailable),
    /// 对话太长，分类器的请求放不下。
    TranscriptTooLong,
    /// 请求被取消（回合停了）。
    Cancelled,
}

/// 请求失败的事实：给模型的说明按它写（哪个模型、为什么、要等多久）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    /// 分类器用的模型。
    pub model: String,
    pub cause: UnavailableCause,
    /// 服务端给的等待时间。
    pub retry_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableCause {
    RateLimited,
    Overloaded,
    ServerError,
    TimedOut,
    ConnectionFailed,
    Other,
}

/// 一个工具调用给分类器看的内容（对照实现的 `toAutoClassifierInput`）。
#[derive(Debug, Clone, PartialEq)]
pub enum ClassifierInput {
    /// 没有要判的内容：摘录里不写这一行；要判的动作是它时直接放行。
    Skip,
    Text(String),
    Json(Value),
}

impl ClassifierInput {
    /// 一段文字；空串同对照实现的空投影，是 [`Self::Skip`]。
    pub fn text(text: String) -> Self {
        if text.is_empty() {
            Self::Skip
        } else {
            Self::Text(text)
        }
    }

    /// 投影要的字段不在输入里时（输入不合 schema）：原始输入的 JSON 文本。
    pub fn raw(input: &Value) -> Self {
        Self::Text(rust_cli_base::js::json::stringify(input))
    }

    /// 字符串字段；没有或不是字符串时是原始输入。
    pub fn field(input: &Value, key: &str) -> Self {
        input
            .get(key)
            .and_then(Value::as_str)
            .map_or_else(|| Self::raw(input), |text| Self::text(text.to_owned()))
    }

    /// 几个字段里第一个有的（JS 的 `a ?? b ?? ""`）；都没有时是 [`Self::Skip`]。
    pub fn first_of(input: &Value, keys: &[&str]) -> Self {
        keys.iter()
            .find_map(|key| input.get(*key).filter(|value| !value.is_null()))
            .map_or(Self::Skip, |value| Self::text(js_text(value)))
    }
}

const SECOND_MS: u64 = 1_000;
const MINUTE_MS: u64 = 60_000;
/// 两分钟以下按秒说。
const SECONDS_BELOW_MS: u64 = 2 * MINUTE_MS;
const MINUTES_PER_HOUR: u64 = 60;
const MINUTES_PER_DAY: u64 = 1_440;
/// 小时数保留一位小数：分钟数除以 6 得到十分之一小时的个数。
const MINUTES_PER_TENTH_HOUR: u64 = 6;
const TENTHS: u64 = 10;

/// 服务端要分类器等多久的说法（分类器的理由与判决链给模型的说明共用）：2 分钟以下按秒、1 小时以下按分钟、一天以内按小时
/// （一位小数），更长是 `more than a day`。
pub fn wait_text(ms: u64) -> String {
    if ms < SECONDS_BELOW_MS {
        let seconds = ms.saturating_add(SECOND_MS / 2) / SECOND_MS;
        let seconds = seconds.max(1);
        let unit = if seconds == 1 { "second" } else { "seconds" };
        return format!("about {seconds} {unit}");
    }
    let minutes = ms.saturating_add(MINUTE_MS / 2) / MINUTE_MS;
    if minutes < MINUTES_PER_HOUR {
        return format!("about {minutes} minutes");
    }
    if minutes > MINUTES_PER_DAY {
        return "more than a day".to_owned();
    }
    let tenths = minutes.saturating_add(MINUTES_PER_TENTH_HOUR / 2) / MINUTES_PER_TENTH_HOUR;
    let hours = if tenths.is_multiple_of(TENTHS) {
        format!("{}", tenths / TENTHS)
    } else {
        format!("{}.{}", tenths / TENTHS, tenths % TENTHS)
    };
    let unit = if tenths == TENTHS { "hour" } else { "hours" };
    format!("about {hours} {unit}")
}

/// 一个值写进 JS 模板字符串或 `String()` 之后的文字：字符串原样，数字、布尔与 null 同 JSON，对象与数组写成 JSON（对照实现的 MCP
/// 投影这样写对象，其余投影只给字符串与数字）。
pub fn js_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => rust_cli_base::js::json::stringify(value),
    }
}

/// 这一步的对话摘录（auto-classifier.md §15.5）：主循环发给模型的消息，加同一个回复里排在这个调用前面的调用。
#[derive(Debug, Clone)]
pub struct RunTranscript {
    pub history: Arc<[Message]>,
    pub earlier: Vec<EarlierCall>,
}

/// 同一个回复里更早的一个调用（还没有提交）。
#[derive(Debug, Clone, PartialEq)]
pub struct EarlierCall {
    /// 它的 tool_use ID：判过的调用在摘录里另起一块（auto-classifier.md §16.15）。
    pub id: crate::ids::ToolUseId,
    pub name: String,
    pub input: Value,
}

#[async_trait]
pub trait AutoClassifier: Send + Sync {
    async fn classify(&self, request: ClassifyRequest<'_>) -> ClassifierVerdict;
}

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod tests;
