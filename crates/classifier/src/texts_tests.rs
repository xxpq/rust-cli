use super::*;

/// 第二阶段失败的理由带等待时间（说法在 kernel-api 的 `wait_text`，那边测）。
#[test]
fn the_stage_two_error_says_how_long_to_wait() {
    assert!(stage_two_error_waiting(30_000).contains("wait about 30 seconds from now"));
}
