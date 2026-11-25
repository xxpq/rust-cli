//! MCP 服务器向用户要的东西（docs/specs/rust-cli/ext.md §3.13）：elicitation 的表单与网址、sampling 的确认。
//!
//! 内核的询问（[`crate::orchestration::InteractionRequest::McpElicitation`]、`McpSampling`）带这里的类型：ext 把服务器的请求
//! 换成它们（协议的解析用 rmcp 的类型，只在 ext），前端画、收回答，回答再由 ext 换回协议的结果。表单的校验只在这里一处：
//! TUI 在输入时用它提示，v5 前端在收到应答时用它拒绝不合格的内容，两边的规则与文字相同。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// 服务器要的一份输入（`elicitation/create`，ext.md §3.13.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Elicitation {
    /// 配置里的服务器名。
    pub server: String,
    /// 服务器给用户看的话。
    pub message: String,
    pub kind: ElicitationKind,
}

/// 两种模式（MCP `2025-11-25`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ElicitationKind {
    /// 表单：字段按服务器给的顺序。
    Form { fields: Vec<FormField> },
    /// 网址：用户同意后在浏览器里打开（敏感的输入不经客户端，ext.md §3.13.4）。
    Url { url: String },
}

/// 表单的一个字段（`requestedSchema.properties` 的一项）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    /// 属性名：回答的 `content` 用它作键。
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 在 `requestedSchema.required` 里。
    pub required: bool,
    pub kind: FieldKind,
}

/// 字段的类型与约束（MCP 只许这几种原始类型）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<TextFormat>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_length: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_length: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    Number {
        /// `type: "integer"`。
        integer: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<f64>,
    },
    Boolean {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<bool>,
    },
    /// 单选（`enum`、带标题的 `oneOf`）。
    Choice {
        options: Vec<Choice>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    /// 多选（`type: "array"`，`items` 是 `enum` 或带标题的 `anyOf`）。
    Choices {
        options: Vec<Choice>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_items: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_items: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        default: Vec<String>,
    },
}

/// 一个选项：回答里用 `value`，界面上写 `label`（没有标题时与 `value` 相同）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

/// 文本字段的格式（MCP 只许这四种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

/// 校验的错误文字（ext.md §3.13.3）。
mod texts {
    pub(super) const EMAIL: &str = "Must be a valid email address, e.g. user@example.com";
    pub(super) const URI: &str = "Must be a valid URI, e.g. https://example.com";
    /// 不认自然语言的日期，两句里只有格式的例子（ext.md §3.13.3）。
    pub(super) const DATE: &str = "Must be a valid date, e.g. 2024-03-15";
    pub(super) const DATE_TIME: &str = "Must be a valid date-time, e.g. 2024-03-15T14:30:00Z";
    pub(super) const REQUIRED: &str = "This field is required";
    pub(super) const NOT_TEXT: &str = "Must be text";
    pub(super) const NOT_BOOLEAN: &str = "Must be true or false";
    pub(super) const UNKNOWN_FIELD: &str = "Is not a field of this form";
    /// 几个错误之间。
    pub(super) const JOIN: &str = "; ";
}

/// 一个字段的值不合格：字段名与给用户看的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl FormField {
    /// 界面上的名字：标题，没有时是属性名。
    pub fn label(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }

    /// 打开表单时的初值（schema 的 `default`）。
    pub fn initial(&self) -> Option<Value> {
        match &self.kind {
            FieldKind::Text { default, .. } => default.clone().map(Value::String),
            FieldKind::Number { default, .. } => {
                default.and_then(Number::from_f64).map(Value::Number)
            }
            FieldKind::Boolean { default } => default.map(Value::Bool),
            FieldKind::Choice { default, .. } => default.clone().map(Value::String),
            FieldKind::Choices { default, .. } if !default.is_empty() => Some(Value::Array(
                default.iter().cloned().map(Value::String).collect(),
            )),
            FieldKind::Choices { .. } => None,
        }
    }

    /// 校验一个值并写成服务器要的类型：文本是字符串；数字可以是数字或数字的文字（输入框给的是文字），
    /// 写成 JSON 数字；布尔可以是布尔或 `"true"` / `"false"`；单选是选项的值；多选是选项值的数组。不合格时是给用户看的原因，
    /// 几条原因用 `; ` 接起。
    pub fn check(&self, value: &Value) -> Result<Value, String> {
        match &self.kind {
            FieldKind::Text {
                format,
                min_length,
                max_length,
                ..
            } => {
                let Value::String(text) = value else {
                    return Err(texts::NOT_TEXT.to_owned());
                };
                check_text(text, *format, *min_length, *max_length).map(|()| value.clone())
            }
            FieldKind::Number {
                integer,
                minimum,
                maximum,
                ..
            } => check_number(value, *integer, *minimum, *maximum),
            FieldKind::Boolean { .. } => match value {
                Value::Bool(_) => Ok(value.clone()),
                Value::String(text) if text == "true" => Ok(Value::Bool(true)),
                Value::String(text) if text == "false" => Ok(Value::Bool(false)),
                _ => Err(texts::NOT_BOOLEAN.to_owned()),
            },
            FieldKind::Choice { options, .. } => match value {
                Value::String(text) if options.iter().any(|option| option.value == *text) => {
                    Ok(value.clone())
                }
                _ => Err(one_of(options)),
            },
            FieldKind::Choices {
                options,
                min_items,
                max_items,
                ..
            } => check_choices(value, options, *min_items, *max_items),
        }
    }
}

/// 整份回答：每个字段按 [`FormField::check`] 写成服务器要的类型；没给、给了 `null` 或空文字的字段当作没填，必填的报错；
/// 表单里没有的键报错（命令严格）。第一个不合格的字段就是结果。
pub fn accept(
    fields: &[FormField],
    content: &Map<String, Value>,
) -> Result<Map<String, Value>, FieldError> {
    if let Some(unknown) = content
        .keys()
        .find(|key| !fields.iter().any(|field| &field.name == *key))
    {
        return Err(FieldError {
            field: unknown.clone(),
            message: texts::UNKNOWN_FIELD.to_owned(),
        });
    }
    let mut accepted = Map::new();
    for field in fields {
        let value = content.get(&field.name).filter(|value| !blank(value));
        let Some(value) = value else {
            if field.required {
                return Err(FieldError {
                    field: field.name.clone(),
                    message: texts::REQUIRED.to_owned(),
                });
            }
            continue;
        };
        let checked = field.check(value).map_err(|message| FieldError {
            field: field.name.clone(),
            message,
        })?;
        accepted.insert(field.name.clone(), checked);
    }
    Ok(accepted)
}

/// 没填：`null`、空文字。
fn blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

/// `1 character` / `n characters`。
fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn check_text(
    text: &str,
    format: Option<TextFormat>,
    min_length: Option<u32>,
    max_length: Option<u32>,
) -> Result<(), String> {
    // 长度按 JS 的 `length`（UTF-16 码元）。
    let length = text.encode_utf16().count();
    let mut issues = Vec::new();
    if let Some(min) = min_length
        && length < min as usize
    {
        issues.push(format!(
            "Must be at least {}",
            plural(min.into(), "character")
        ));
    }
    if let Some(max) = max_length
        && length > max as usize
    {
        issues.push(format!(
            "Must be at most {}",
            plural(max.into(), "character")
        ));
    }
    let format_issue = match format {
        Some(TextFormat::Email) if !formats::email(text) => Some(texts::EMAIL),
        Some(TextFormat::Uri) if !formats::uri(text) => Some(texts::URI),
        Some(TextFormat::Date) if !formats::date(text) => Some(texts::DATE),
        Some(TextFormat::DateTime) if !formats::date_time(text) => Some(texts::DATE_TIME),
        _ => None,
    };
    issues.extend(format_issue.map(str::to_owned));
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues.join(texts::JOIN))
    }
}

/// 数字的写法：整数字段写整数，数字字段里的整数写成 `n.0`。
fn number_text(n: f64, integer: bool) -> String {
    if n.fract() == 0.0 && !integer {
        format!("{n:.1}")
    } else {
        format!("{n}")
    }
}

/// 数字不合格时只给一句话，说清类型与范围。
fn range_message(integer: bool, minimum: Option<f64>, maximum: Option<f64>) -> String {
    let kind = if integer { "an integer" } else { "a number" };
    let text = |n: f64| number_text(n, integer);
    match (minimum, maximum) {
        (Some(min), Some(max)) => format!("Must be {kind} between {} and {}", text(min), text(max)),
        (Some(min), None) => format!("Must be {kind} >= {}", text(min)),
        (None, Some(max)) => format!("Must be {kind} <= {}", text(max)),
        (None, None) => format!("Must be {kind}"),
    }
}

fn check_number(
    value: &Value,
    integer: bool,
    minimum: Option<f64>,
    maximum: Option<f64>,
) -> Result<Value, String> {
    let wrong = || range_message(integer, minimum, maximum);
    let n = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
    .ok_or_else(wrong)?;
    let in_range = minimum.is_none_or(|min| n >= min) && maximum.is_none_or(|max| n <= max);
    if (integer && n.fract() != 0.0) || !in_range {
        return Err(wrong());
    }
    // 整数写成 JSON 的整数（服务器按 integer 解析，JS 的数字也不分 3 与 3.0）；绝对值小于 2^53 的整数 `as` 是精确的，更大的照原值写。
    if n.fract() == 0.0 && n.abs() < I64_EXACT {
        return Ok(Value::Number(Number::from(n as i64)));
    }
    Number::from_f64(n).map(Value::Number).ok_or_else(wrong)
}

/// f64 能精确表示的整数的上限（2^53）。
const I64_EXACT: f64 = 9_007_199_254_740_992.0;

/// zod 4 的 `z.enum` 不合格时的原文（单选与多选都用它）。
fn one_of(options: &[Choice]) -> String {
    let values: Vec<String> = options
        .iter()
        .map(|option| format!("\"{}\"", option.value))
        .collect();
    format!("Invalid option: expected one of {}", values.join("|"))
}

fn check_choices(
    value: &Value,
    options: &[Choice],
    min_items: Option<u64>,
    max_items: Option<u64>,
) -> Result<Value, String> {
    let Value::Array(items) = value else {
        return Err(one_of(options));
    };
    if !items.iter().all(|item| {
        item.as_str()
            .is_some_and(|text| options.iter().any(|option| option.value == text))
    }) {
        return Err(one_of(options));
    }
    let count = items.len() as u64;
    if let Some(min) = min_items
        && count < min
    {
        return Err(format!("Select at least {}", plural(min, "item")));
    }
    if let Some(max) = max_items
        && count > max
    {
        return Err(format!("Select at most {}", plural(max, "item")));
    }
    Ok(value.clone())
}

/// 四种格式的判断，照 zod 4 的规则（ext.md §3.13.3）。
mod formats {
    use std::sync::LazyLock;

    /// zod 4 的 `email`：本地部分不以 `.` 开头、没有连续的 `..`，最后一个字符不是 `.`；域名每段以字母数字开头，顶级域至少两个字母。
    /// zod 的写法带先行断言，这里用 JS 语义的 regress 照原文。
    static EMAIL: LazyLock<regress::Regex> = LazyLock::new(|| {
        #[expect(clippy::expect_used, reason = "固定的正则")]
        regress::Regex::new(
            r"^(?!\.)(?!.*\.\.)([A-Za-z0-9_'+\-\.]*)[A-Za-z0-9_+-]@([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$",
        )
        .expect("the email pattern is valid")
    });
    /// zod 4 的 `date`：`YYYY-MM-DD`，月与日在合法范围之内（含闰年的 2 月 29 日）。
    static DATE: LazyLock<regress::Regex> = LazyLock::new(|| {
        #[expect(clippy::expect_used, reason = "固定的正则")]
        regress::Regex::new(
            r"^(?:(?:\d\d[2468][048]|\d\d[13579][26]|\d\d0[48]|[02468][048]00|[13579][26]00)-02-29|\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\d|30)|(?:02)-(?:0[1-9]|1\d|2[0-8])))$",
        )
        .expect("the date pattern is valid")
    });
    /// zod 4 的 `datetime({offset: true})`：日期、`T`、`HH:MM`（秒与小数秒可选）、`Z` 或 `±HH:MM`（冒号可省）。
    static TIME_TAIL: LazyLock<regress::Regex> = LazyLock::new(|| {
        #[expect(clippy::expect_used, reason = "固定的正则")]
        regress::Regex::new(
            r"^T(?:[01]\d|2[0-3]):[0-5]\d(?::[0-5]\d(?:\.\d+)?)?(?:Z|[+-](?:[01]\d|2[0-3])(?::?[0-5]\d)?)$",
        )
        .expect("the time pattern is valid")
    });

    pub(super) fn email(text: &str) -> bool {
        EMAIL.find(text).is_some()
    }

    /// zod 4 的 `url`：`new URL(text)` 解析得出，且主机名不为空的协议才要求主机（WHATWG URL，url 库即此规范的实现）。
    pub(super) fn uri(text: &str) -> bool {
        url::Url::parse(text).is_ok()
    }

    pub(super) fn date(text: &str) -> bool {
        DATE.find(text).is_some()
    }

    /// `YYYY-MM-DD` 的长度（ASCII）：日期时间的日期部分。
    const DATE_LEN: usize = 10;

    pub(super) fn date_time(text: &str) -> bool {
        text.get(..DATE_LEN)
            .zip(text.get(DATE_LEN..))
            .is_some_and(|(day, time)| date(day) && TIME_TAIL.find(time).is_some())
    }
}

/// 回给服务器的三种动作（MCP `ElicitResult.action`；hook 的 `hookSpecificOutput.action` 也是它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}

impl ElicitationAction {
    /// 线上的写法（hooks 的输入与 Notification 的文字）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

/// elicitation 的回答（[`crate::interaction::Answer::McpElicitation`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ElicitationAnswer {
    /// 提交：表单是按 [`accept`] 校验过的内容；网址模式没有内容（用户同意打开）。
    Accept {
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        content: Map<String, Value>,
    },
    /// 明确拒绝。
    Decline,
    /// 不做选择就关掉（esc、停下回合、没有人能回答）。
    Cancel,
}

impl ElicitationAnswer {
    pub fn action(&self) -> ElicitationAction {
        match self {
            Self::Accept { .. } => ElicitationAction::Accept,
            Self::Decline => ElicitationAction::Decline,
            Self::Cancel => ElicitationAction::Cancel,
        }
    }
}

/// 服务器要借用模型（`sampling/createMessage`，ext.md §3.14）：给用户看的部分。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sampling {
    pub server: String,
    /// 服务器给的对话，按顺序；图片写成占位（只给人看，发给模型的是原样的内容）。
    pub messages: Vec<SamplingMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// 实际给模型的输出上限（服务器要的与本工程上限中小的那个）。
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SamplingMessage {
    pub role: SamplingRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingRole {
    User,
    Assistant,
}

/// sampling 确认的回答（[`crate::interaction::Answer::McpSampling`]）。名字与别的回答不重（`Answer` 是 untagged）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum SamplingAnswer {
    /// 同意这一次；`session` 为真时这个会话里这个服务器之后的请求不再问（ext.md §3.14.2）。
    SamplingAllowed {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        session: bool,
    },
    SamplingDenied,
}

#[cfg(test)]
#[path = "mcp_ask_tests.rs"]
mod tests;
