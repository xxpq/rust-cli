use std::sync::Arc;

use rust_cli_kernel_api::ids::{AgentId, SessionId};
use tokio_util::sync::CancellationToken;

use super::*;

fn main_lane() -> LaneKey {
    (SessionId::new("s1"), None)
}

/// 同一个键一个一个来；放掉之后下一个拿到，表项在没人用时删掉。
#[tokio::test]
async fn one_turn_per_lane_and_the_table_empties() {
    let lanes = Arc::new(Lanes::default());
    let cancel = CancellationToken::new();
    let first = lanes.enter(main_lane(), &cancel).await.unwrap();
    let waiting = {
        let lanes = lanes.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { lanes.enter(main_lane(), &cancel).await.is_some() })
    };
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    // 别的代理不排在这条队列里
    let other = lanes
        .enter((SessionId::new("s1"), Some(AgentId::new("a1"))), &cancel)
        .await;
    assert!(other.is_some());
    drop(other);
    drop(first);
    assert!(waiting.await.unwrap());
    assert_eq!(lanes.len(), 0);
}

/// 等的时候取消：不拿许可，表项照样清掉。
#[tokio::test]
async fn cancelling_while_waiting_gives_no_turn() {
    let lanes = Arc::new(Lanes::default());
    let held = lanes
        .enter(main_lane(), &CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let waiting = {
        let lanes = lanes.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { lanes.enter(main_lane(), &cancel).await.is_some() })
    };
    tokio::task::yield_now().await;
    cancel.cancel();
    assert!(!waiting.await.unwrap());
    drop(held);
    assert_eq!(lanes.len(), 0);
}
