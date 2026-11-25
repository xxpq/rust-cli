//! 回复的解析（docs/specs/rust-cli/auto-classifier.md §15.7）：分数、`<block>`、理由与类别，以及“解析不了”是不是拒绝。

use std::sync::LazyLock;

use regex::Regex;
use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::side_request::SideReply;

use crate::prompt::{RuleNames, known};

/// 固定的正则。
fn fixed(pattern: &str) -> Regex {
    #[expect(clippy::expect_used, reason = "固定的正则")]
    Regex::new(pattern).expect("valid classifier reply pattern")
}

/// 完整的 `<thinking>…</thinking>`。
static THINKING: LazyLock<Regex> = LazyLock::new(|| fixed(r"<thinking>[\s\S]*?</thinking>"));
/// 没有结尾的 `<thinking>` 到全文末尾。
static OPEN_THINKING: LazyLock<Regex> = LazyLock::new(|| fixed(r"<thinking>[\s\S]*$"));
static SEVERITY: LazyLock<Regex> =
    LazyLock::new(|| fixed(r"<severity>\s*(\d+(?:\.\d+)?)\s*(?:</severity>)?"));
/// `\b` 照 JS（没有 `u` 标志）只认 ASCII 的词边界。
static BLOCK: LazyLock<Regex> = LazyLock::new(|| fixed(r"(?i)<block>(yes|no)(?-u:\b)"));
static REASON: LazyLock<Regex> = LazyLock::new(|| fixed(r"<reason>([\s\S]*?)</reason>"));
static CATEGORY: LazyLock<Regex> =
    LazyLock::new(|| fixed(r"(?i)<category>([a-z0-9 &_-]{1,64})</category>"));

/// 去掉 thinking（完整的与没有结尾的）之后的文字。
fn without_thinking(text: &str) -> String {
    let complete = THINKING.replace_all(text, "");
    OPEN_THINKING.replace_all(&complete, "").into_owned()
}

/// 分数：结束原因要是停止序列或正常结束；去掉完整的 thinking 之后不能还有 `<thinking>`；`<severity>N` 恰好一个。
pub(crate) fn severity(text: &str, stop: Option<StopReason>) -> Option<f64> {
    if !matches!(stop, Some(StopReason::StopSequence | StopReason::EndTurn)) {
        return None;
    }
    let stripped = THINKING.replace_all(text, "");
    if stripped.contains("<thinking>") {
        return None;
    }
    let mut found = SEVERITY.captures_iter(&stripped);
    let first = found.next()?;
    if found.next().is_some() {
        return None;
    }
    first.get(1)?.as_str().parse().ok()
}

/// `<block>yes|no`：全文里出现两种值时解析不了；去掉 thinking 之后至少一个、只有一种值。`Some(true)` 是拦下。
pub(crate) fn block(text: &str) -> Option<bool> {
    let values = |text: &str| -> Vec<bool> {
        BLOCK
            .captures_iter(text)
            .filter_map(|found| found.get(1))
            .map(|value| value.as_str().eq_ignore_ascii_case("yes"))
            .collect()
    };
    let distinct = |values: &[bool]| values.iter().any(|v| *v) && values.iter().any(|v| !*v);
    if distinct(&values(text)) {
        return None;
    }
    let kept = values(&without_thinking(text));
    if kept.is_empty() || distinct(&kept) {
        return None;
    }
    Some(kept[0])
}

/// 第一个 `<reason>` 的内容（去首尾空白）。
pub(crate) fn reason(text: &str) -> Option<String> {
    let stripped = without_thinking(text);
    let found = REASON.captures(&stripped)?;
    Some(found.get(1)?.as_str().trim().to_owned())
}

/// 拦截规则的 slug：小写，非字母数字的连续段换成 `_`，去掉首尾的 `_`。
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// 分数格式拦下时的理由 `[<规则名>]`：`<category>` 恰好一个、slug 认得；规则名取提示里的写法，没有时把非字母数字换成空格。
pub(crate) fn category(text: &str, names: &RuleNames) -> Option<String> {
    let stripped = without_thinking(text);
    let mut found = CATEGORY.captures_iter(&stripped);
    let name = found.next()?.get(1)?.as_str().to_owned();
    if found.next().is_some() {
        return None;
    }
    let key = slug(&name);
    if !known(&key) {
        return None;
    }
    let shown = names.name(&key).map_or_else(
        || {
            name.split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        },
        str::to_owned,
    );
    Some(format!("[{shown}]"))
}

/// 解析不了的回复是不是拒绝：结束原因是拒绝，或者没有文字且不是因为输出上限。拒绝不重发。
pub(crate) fn refused(reply: &SideReply) -> bool {
    reply.stop_reason == Some(StopReason::Refusal)
        || (reply.text().is_empty()
            && !matches!(
                reply.stop_reason,
                Some(StopReason::MaxTokens | StopReason::ModelContextWindowExceeded)
            ))
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
