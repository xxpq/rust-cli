//! 处置表的 P0、P1 两列（provider-errors.md §2.4）：每一格一条。

use super::{Action, Exhausted, Phase, dispose};
use crate::errors::Class;

const RETRY: Action = Action::Retry {
    exhausted: Exhausted::Fail,
};

/// （分类，P0 的动作，P1 的动作），来源是前台、没有 `x-should-retry`。
const TABLE: [(Class, Action, Action); 18] = [
    (Class::Overloaded, Action::Overload, Action::Overload),
    (Class::RateLimited, RETRY, RETRY),
    (Class::CapacityLimited, Action::Capacity, Action::Capacity),
    (Class::ServerError, RETRY, Action::Resend),
    (
        Class::Network,
        RETRY,
        Action::Retry {
            exhausted: Exhausted::Resend,
        },
    ),
    (Class::TlsFailure, Action::Fail, Action::Fail),
    (Class::Malformed, Action::Fail, Action::Resend),
    (Class::Truncated, Action::Fail, Action::Truncation),
    (Class::AuthRejected, RETRY, Action::Fail),
    // 登录失效：两个阶段都是“续期一次”（能不能续期由执行点看端点，主会话审定 2026-11-06）。
    (Class::LoginExpired, Action::Renew, Action::Renew),
    (Class::AccessDenied, Action::Fail, Action::Fail),
    // 流式请求的 404：响应头之前改非流式（G，PROVIDER-RETRY，provider-errors.md §3.6）。
    (Class::NotFound, Action::Resend, Action::Fail),
    // 空回复：两个阶段都重试一次（§3.7）。
    (Class::EmptyReply, Action::Empty, Action::Empty),
    (Class::QuotaExhausted, Action::Fail, Action::Fail),
    (Class::ContentBlocked, Action::Fail, Action::Fail),
    (Class::ContextOverflow, Action::Fail, Action::Fail),
    (Class::TooLarge, Action::Fail, Action::Fail),
    // 流里来的参数错误不走 G（D15 暂定）。
    (Class::InvalidRequest, Action::Fail, Action::Fail),
];

#[test]
fn every_cell_of_the_table() {
    for (class, head, stream) in TABLE {
        assert_eq!(
            dispose(class, Phase::Head, true, None),
            head,
            "{class:?} P0"
        );
        assert_eq!(
            dispose(class, Phase::Stream, true, None),
            stream,
            "{class:?} P1"
        );
    }
}

/// 后台来源过载时不重试：P0 直接失败；P1 改非流式（D19 改：以前 P1 也直接失败）。别的分类不看来源。
#[test]
fn background_sources_do_not_retry_overload() {
    assert_eq!(
        dispose(Class::Overloaded, Phase::Head, false, None),
        Action::Fail
    );
    assert_eq!(
        dispose(Class::Overloaded, Phase::Stream, false, None),
        Action::Resend
    );
    for phase in [Phase::Head, Phase::Stream] {
        assert_eq!(dispose(Class::RateLimited, phase, false, None), RETRY);
    }
}

/// `x-should-retry` 只在 P0：过载先认、不看它；其余分类先看它，再按表。
#[test]
fn the_retry_header_comes_before_the_table() {
    assert_eq!(
        dispose(Class::Overloaded, Phase::Head, true, Some(false)),
        Action::Overload
    );
    assert_eq!(
        dispose(Class::AuthRejected, Phase::Head, true, Some(false)),
        Action::Fail
    );
    assert_eq!(
        dispose(Class::InvalidRequest, Phase::Head, true, Some(true)),
        RETRY
    );
    assert_eq!(
        dispose(Class::ServerError, Phase::Stream, true, Some(false)),
        Action::Resend
    );
}
