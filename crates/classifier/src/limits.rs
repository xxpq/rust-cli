//! 请求的数值（docs/specs/rust-cli/auto-classifier.md §15.6）：输出上限、重试与重发的次数、请求时限。

use std::time::Duration;

/// 第一阶段的输出上限：只要一个分数或 yes/no。
pub(crate) const STAGE_ONE_MAX_TOKENS: u32 = 64;
/// 第二阶段的输出上限：带 `<thinking>`。
pub(crate) const STAGE_TWO_MAX_TOKENS: u32 = 8_192;
/// 不能关 thinking 的模型另加的输出余量。
pub(crate) const ALWAYS_THINKING_EXTRA_TOKENS: u32 = 2_048;

/// 模型端口里一次请求最多重试几次（请求级的上限，`RequestOptions::max_retries`）。
pub(crate) const API_RETRIES: u32 = 4;
/// 回复解析不了时最多再发几次。
pub(crate) const RESENDS: u32 = 4;

/// 一次尝试的基本时限。
const ATTEMPT_BASE: Duration = Duration::from_secs(60);
/// 估算的 token 数每多这么多，第一阶段的时限加一档。
const TOKENS_PER_STEP: u64 = 50_000;
/// 每一档加的时间。
const STEP: Duration = Duration::from_secs(10);
/// 第二阶段整个阶段的时限，也是第一阶段时限的上限。
pub(crate) const STAGE_TWO_LIMIT: Duration = Duration::from_secs(120);
/// 第二阶段每次尝试的时限至少这么长。
const STAGE_TWO_ATTEMPT_FLOOR: Duration = Duration::from_secs(60);
/// 字符数换 token 数的估算。
const CHARS_PER_TOKEN: usize = 4;

/// 一个阶段的时限：每次尝试多长（端口每发一次重试通知重新计时）、重发只在开始后多久之内开始、整个阶段多长。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StageLimits {
    pub(crate) attempt: Duration,
    pub(crate) resend_window: Duration,
    pub(crate) ceiling: Duration,
}

/// 估算的 token 数：提示与摘录的字符数除以 4（四舍五入）。
pub(crate) fn estimate_tokens(chars: usize) -> u64 {
    u64::try_from(chars.saturating_add(CHARS_PER_TOKEN / 2) / CHARS_PER_TOKEN).unwrap_or(u64::MAX)
}

/// 第一阶段一次尝试的时限：5 万 token 以下 60 s，每多 5 万加 10 s，最多 120 s。
pub(crate) fn attempt_limit(tokens: u64) -> Duration {
    let steps = tokens
        .saturating_sub(TOKENS_PER_STEP)
        .div_ceil(TOKENS_PER_STEP);
    let extra = STEP.saturating_mul(u32::try_from(steps).unwrap_or(u32::MAX));
    ATTEMPT_BASE.saturating_add(extra).min(STAGE_TWO_LIMIT)
}

pub(crate) fn stage_one(attempt: Duration) -> StageLimits {
    StageLimits {
        attempt,
        resend_window: attempt,
        ceiling: attempt.max(ATTEMPT_BASE.saturating_mul(API_RETRIES + 1)),
    }
}

pub(crate) fn stage_two(attempt: Duration) -> StageLimits {
    StageLimits {
        attempt: attempt.max(STAGE_TWO_ATTEMPT_FLOOR),
        resend_window: STAGE_TWO_LIMIT,
        ceiling: STAGE_TWO_LIMIT,
    }
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
