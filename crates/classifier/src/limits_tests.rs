use std::time::Duration;

use super::*;

/// 请求时限的数值（auto-classifier.md §15.6.4）：5 万 token 以下 60 s，每多 5 万加 10 s，最多 120 s。
#[test]
fn the_attempt_limit_grows_with_the_transcript() {
    assert_eq!(attempt_limit(0), Duration::from_secs(60));
    assert_eq!(attempt_limit(50_000), Duration::from_secs(60));
    assert_eq!(attempt_limit(50_001), Duration::from_secs(70));
    assert_eq!(attempt_limit(150_000), Duration::from_secs(80));
    assert_eq!(attempt_limit(10_000_000), Duration::from_secs(120));
    assert_eq!(estimate_tokens(10), 3);
    assert_eq!(estimate_tokens(9), 2);
}

/// 第一阶段：重发在时限之内开始，整个阶段最多 (4 + 1) × 60 s；第二阶段：每次至少 60 s，整个阶段 120 s。
#[test]
fn the_stage_limits_follow_the_first_attempt_limit() {
    let one = stage_one(Duration::from_secs(70));
    assert_eq!(one.attempt, Duration::from_secs(70));
    assert_eq!(one.resend_window, Duration::from_secs(70));
    assert_eq!(one.ceiling, Duration::from_secs(300));
    let two = stage_two(Duration::from_secs(70));
    assert_eq!(two.attempt, Duration::from_secs(70));
    assert_eq!(two.ceiling, Duration::from_secs(120));
    assert_eq!(
        stage_two(Duration::from_secs(60)).attempt,
        Duration::from_secs(60)
    );
}
