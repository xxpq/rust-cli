//! `state.usage`（protocol-v5.md §50.3）：上下文环的上限是模型的标称窗口（照 Node），不知道窗口时 `contextWindow: null`。

use rust_cli_kernel_api::compaction::ContextOccupancy;
use serde_json::json;

use super::usage;

fn occupancy(window: Option<u64>) -> ContextOccupancy {
    ContextOccupancy {
        used_tokens: 46,
        max_tokens: 980_000,
        compact_at: Some(967_000),
        window,
    }
}

/// 修 bug（X2，protocol-v5.md §50.3）：以前 `max` 是有效窗口（1M 的模型 980000），v4 是标称窗口（1000000）；用户暂定照 Node。
#[test]
fn the_ring_max_is_the_nominal_window() {
    assert_eq!(
        usage(Some(&occupancy(Some(1_000_000))))["contextWindow"],
        json!({"used": 46, "max": 1_000_000, "autoCompactThreshold": 967_000})
    );
}

/// 不知道窗口的模型不画上限（v4 的 `contextWindow: null`，E2E CW06「未知模型不应显示伪造容量」）；没有用量时同样是 null。
#[test]
fn an_unknown_window_has_no_ring() {
    assert_eq!(usage(Some(&occupancy(None)))["contextWindow"], json!(null));
    assert_eq!(usage(None)["contextWindow"], json!(null));
}
