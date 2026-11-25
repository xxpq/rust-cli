//! 第 2 层：处置表的 P0、P1 两列（docs/specs/rust-cli/provider-errors.md §2.4，修复线 F113）。
//!
//! 纯函数：只按中性分类、阶段与来源给出动作。计数与上限由执行点（transport 的 `Connect`）按动作去做，每个计数只有它
//! 一个 owner（§4）。P2t、P2 与结束原因由 query 按 `ModelError` / `StopReason` 判断（同一张表的后两列）。

use super::Class;

/// 失败发生在哪个阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// P0：拿到响应头之前（HTTP 非 2xx、传输错误）。
    Head,
    /// P1：流已开始，还没越过交出边界（调用方一个内容事件都还没收到）。
    Stream,
}

/// transport 对一次失败做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// 退避后重发（`attempt` 计数）；次数用尽时按 `exhausted`。
    Retry { exhausted: Exhausted },
    /// 过载：计入过载次数；第 3 次有 fallback 时换模型，否则 P0 按模型系列、P1 走 G（`Connect` 做）。
    Overload,
    /// 账号或套餐的并发上限：只有 Start Plan 主回合的准入短等重试 2 次，别的请求不重试（`Connect` 按请求判断，
    /// provider-errors.md §5.3.2）。
    Capacity,
    /// 登录失效：端点能续期时续期一次再发，否则不重试（`Connect` 看端点，§5.3.1）。
    Renew,
    /// 截断：重试 1 次，之后走 G（D17 暂定）。
    Truncation,
    /// 空回复：重试 1 次（退避，计入次数），之后交给调用方（provider-errors.md §3.7）。
    Empty,
    /// G：改用非流式请求重发一次，之后这次调用里的请求都是非流式（provider-errors.md §3.6；F113 是一次流式重发）。
    Resend,
    /// 不重试，交给调用方（终止、压缩都由 query 按错误做）。
    Fail,
}

/// 重试次数用尽之后。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exhausted {
    Fail,
    Resend,
}

const RETRY: Action = Action::Retry {
    exhausted: Exhausted::Fail,
};

/// 一次失败的动作。`foreground`：用户在等结果的来源（过载时才重试）；`should_retry`：响应头里的 `x-should-retry`
/// （只在 P0 有）。过载先认、不看响应头；其余分类先看这个头，再按表（model.md §5.2 的顺序，现状）。
pub(crate) fn dispose(
    class: Class,
    phase: Phase,
    foreground: bool,
    should_retry: Option<bool>,
) -> Action {
    let header = if phase == Phase::Head {
        should_retry
    } else {
        None
    };
    match (class, phase) {
        (Class::Overloaded, _) if foreground => Action::Overload,
        (Class::Overloaded, Phase::Head) => Action::Fail,
        // 后台来源的流内过载不流式重试，直接改非流式（D19 改：F113 的 G 只是一次流式重发，那时按“不重试”）。
        (Class::Overloaded, Phase::Stream) => Action::Resend,
        // 流式请求的 404 改非流式重发：响应头里的 `x-should-retry` 管的是同样的请求再发不发，不管换成非流式。
        (Class::NotFound, Phase::Head) => Action::Resend,
        (Class::EmptyReply, _) => Action::Empty,
        (Class::LoginExpired, _) => Action::Renew,
        _ if header == Some(true) => RETRY,
        _ if header == Some(false) => Action::Fail,
        (
            Class::RateLimited | Class::ServerError | Class::Network | Class::AuthRejected,
            Phase::Head,
        )
        | (Class::RateLimited, Phase::Stream) => RETRY,
        (Class::CapacityLimited, _) => Action::Capacity,
        (Class::ServerError | Class::Malformed, Phase::Stream) => Action::Resend,
        (Class::Network, Phase::Stream) => Action::Retry {
            exhausted: Exhausted::Resend,
        },
        (Class::Truncated, Phase::Stream) => Action::Truncation,
        // 流里来的参数错误不走 G（D15 暂定）：智谱表外的业务码也归参数错误，按 D1 不重试；要改非流式重发，先把它们分出来。
        (
            Class::TlsFailure
            | Class::AuthRejected
            | Class::AccessDenied
            | Class::NotFound
            | Class::QuotaExhausted
            | Class::ContentBlocked
            | Class::ContextOverflow
            | Class::TooLarge
            | Class::InvalidRequest
            | Class::Malformed
            | Class::Truncated,
            _,
        ) => Action::Fail,
    }
}

#[cfg(test)]
#[path = "dispose_tests.rs"]
mod tests;
