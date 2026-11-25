//! 由视图建投影的规模（MEM-V5，performance.md §13）：长会话打开时建一次投影，耗时随消息数线性涨，不按平方涨。
//!
//! 用户打开几万个工具行的会话时打开「极其极其慢」、内存暴增（performance.md §13.1）。这里用同一种形状的合成视图（每个回合一条输入、
//! 若干次工具调用与结果、一段回答，每个调用带起止时刻）量 N 与 4N 个回合各建一次的耗时：线性时约 4 倍，平方时约 16 倍。

use std::time::{Duration, Instant};

use rust_cli_kernel_api::session_api::SessionView;

use super::kit::{long_view, origin};
use super::*;

/// 每个工具结果的字节数（与内容无关，只让行有分量）。
const OUTPUT_BYTES: usize = 2_000;
/// 小的那一份的回合数；大的那一份是它的 [`SCALE`] 倍。
const BASE_TURNS: usize = 100;
const SCALE: usize = 4;
/// 每种规模建几次取最短的（去掉机器负载的抖动）。
const RUNS: usize = 7;
/// 大的那一份最多是小的这么多倍：线性约 [`SCALE`] 倍，平方约 `SCALE²` = 16 倍；留出常数项与抖动的余量。
const MAX_RATIO: f64 = 10.0;

fn build_time(view: &SessionView) -> Duration {
    let started = Instant::now();
    let (_, content) = Projection::build(view, origin(false));
    let took = started.elapsed();
    drop(content);
    took
}

/// 两种规模交替各建 `RUNS` 次，各取最短的一次。交替是为了让两边碰到同样的机器负载：同进程里别的测试与别的程序在抢 CPU 时，
/// 先连着量小的、再连着量大的，比值会被负载的起伏放大（整批跑时偶发超出过上限）。
fn fastest_builds(small: &SessionView, large: &SessionView) -> (Duration, Duration) {
    (0..RUNS).fold(
        (Duration::MAX, Duration::MAX),
        |(fast_small, fast_large), _| {
            (
                fast_small.min(build_time(small)),
                fast_large.min(build_time(large)),
            )
        },
    )
}

#[test]
fn building_a_long_session_grows_linearly() {
    let small = long_view(BASE_TURNS, OUTPUT_BYTES);
    let large = long_view(BASE_TURNS * SCALE, OUTPUT_BYTES);
    // 每一行都在：规模真的是 SCALE 倍（输入、调用、回答各一行）
    let rows =
        |view: &SessionView| Projection::build(view, origin(false)).1.collections[ROWS].len();
    assert_eq!(rows(&large), rows(&small) * SCALE);
    let (small_took, large_took) = fastest_builds(&small, &large);
    let ratio = large_took.as_secs_f64() / small_took.as_secs_f64().max(f64::EPSILON);
    assert!(
        ratio < MAX_RATIO,
        "{} turns took {small_took:?}, {} turns took {large_took:?} (x{ratio:.1}): building is not linear",
        BASE_TURNS,
        BASE_TURNS * SCALE
    );
}
