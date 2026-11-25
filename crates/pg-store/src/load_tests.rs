//! 工作负载验证；记录测量值，不把本机结果当作托管 PG 的容量承诺。
use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{ForkReceipt, SessionFork, SessionStore};
use rust_cli_kernel_api::testing::selection;
use tokio::task::JoinSet;

use crate::tests::{batch, fixture, meta};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load scenario; run the isolated PG runner with --load"]
async fn thousand_sessions_share_a_bounded_pool() {
    let (database, store) = fixture("load").await;
    let mut pending = JoinSet::new();
    for index in 0..1_000 {
        let store = store.clone();
        pending.spawn(async move {
            let id = format!("session-{index}");
            store
                .create(
                    meta(&id),
                    // 消息 ID 在租户内唯一（Node 的 message 主键，pg-store.md §12.1）。
                    batch(&format!("initial-{index}")),
                )
                .await
                .unwrap();
            SessionId::new(&id)
        });
    }
    let mut sessions = Vec::new();
    while let Some(session) = pending.join_next().await {
        sessions.push(session.unwrap());
    }
    let start = Instant::now();
    let burst_gate = Arc::new(tokio::sync::Barrier::new(1_000));
    let mut pending = JoinSet::new();
    for (index, session) in sessions.into_iter().enumerate() {
        let burst_gate = burst_gate.clone();
        let store = store.clone();
        pending.spawn(async move {
            let mut latencies = Vec::new();
            for round in 0..3 {
                tokio::time::sleep_until(tokio::time::Instant::from_std(
                    start + Duration::from_millis(index as u64 * 5 + round * 5_000),
                ))
                .await;
                let before = Instant::now();
                let id = format!("{index}-{round}");

                store.commit(&session, batch(&id)).await.unwrap();
                latencies.push(before.elapsed().as_micros());
            }
            burst_gate.wait().await;
            let before = Instant::now();
            let id = format!("burst-{index}");
            store.commit(&session, batch(&id)).await.unwrap();
            (latencies, before.elapsed().as_micros())
        });
    }
    let mut latencies = Vec::new();
    let mut burst = Vec::new();
    while let Some(result) = pending.join_next().await {
        let (steady, duration) = result.unwrap();
        latencies.extend(steady);
        burst.push(duration);
    }
    burst.sort_unstable();
    println!(
        "burst_commits=1000 p95_us={} p99_us={}",
        burst[950], burst[990]
    );
    latencies.sort_unstable();
    assert_eq!(latencies.len(), 3_000);
    assert!(database.connections() <= 4);
    println!(
        "sessions=1000 commits={} elapsed_ms={} pool={} p50_us={} p95_us={} p99_us={}",
        latencies.len(),
        start.elapsed().as_millis(),
        database.connections(),
        latencies[1500],
        latencies[2850],
        latencies[2970]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load scenario; run the isolated PG runner with --load"]
async fn large_history_and_fork_are_atomic() {
    let (_, store) = fixture("large-history").await;
    let source = meta("large");
    store
        .create(source.clone(), batch("initial"))
        .await
        .unwrap();
    let mut facts = batch("m0");
    for i in 1..5_000 {
        facts.facts.extend(batch(&format!("m{i}")).facts);
    }
    let before = Instant::now();
    store.commit(&source.id, facts).await.unwrap();
    let write_ms = before.elapsed().as_millis();
    let before = Instant::now();
    let loaded = store.load(&source.id).await.unwrap();
    assert_eq!(loaded.messages.len(), 5_001);
    let load_ms = before.elapsed().as_millis();
    let before = Instant::now();
    let child = rust_cli_kernel_api::ids::SessionId::new("fork");
    let receipt = store
        .fork(SessionFork {
            parent: source.id,
            child: child.clone(),
            through: loaded.messages.last().unwrap().id().clone(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            created_at_ms: 2_000,
        })
        .await
        .unwrap();
    assert_eq!(receipt, ForkReceipt::Created { copied: 5_001 });
    assert_eq!(store.load(&child).await.unwrap().messages.len(), 5_002);
    println!(
        "history=5001 write_ms={write_ms} cold_load_ms={load_ms} fork_ms={}",
        before.elapsed().as_millis()
    );
}
