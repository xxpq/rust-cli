//! 首次连接的并发上限（ext.md §3.3.4）：一次 `start` 里本机（stdio）最多 3 个、远端最多 20 个同时握手；alwaysLoad 另有一套；
//! 重连不占名额。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::manage::McpAction;
use serde_json::json;

use super::*;
use crate::mcp::McpScope;
use crate::test_support::{FakeMcp, held, server};

fn config(name: &str, remote: bool, always_load: bool) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::User,
        transport: if remote {
            McpTransportConfig::http("https://mcp.example.com/mcp".to_owned(), Default::default())
        } else {
            McpTransportConfig::stdio("node".to_owned(), Vec::new())
        },
        options: McpServerOptions {
            always_load,
            ..Default::default()
        },
        machine: Default::default(),
    }
}

fn quiet() -> Arc<crate::test_support::Handler> {
    server(json!({}), None, |_, _| None)
}

fn initializing(fake: &FakeMcp) -> Vec<String> {
    let mut names: Vec<String> = fake
        .received()
        .into_iter()
        .filter(|(_, message)| message["method"] == "initialize")
        .map(|(name, _)| name)
        .collect();
    names.sort();
    names
}

/// 让连接任务跑到它们会停下的地方（等名额或等握手）。
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// 等到有 `count` 个本机普通服务器在握手（有次数上限，超过就当作没有）。
async fn local_initializing(fake: &FakeMcp, count: usize) -> usize {
    let mut seen = 0;
    for _ in 0..20 {
        settle().await;
        seen = initializing(fake)
            .iter()
            .filter(|name| name.starts_with('l'))
            .count();
        if seen >= count {
            break;
        }
    }
    seen
}

#[tokio::test]
async fn first_connections_are_limited_per_kind() {
    let mut fake = FakeMcp::default();
    for name in ["l1", "l2", "l3", "l4", "l5", "r1", "r2", "always"] {
        fake = fake.with(name, held(|_, _| None));
    }
    let fake = Arc::new(fake);
    let scope = TaskScope::root("mcp-batch", |_| {});
    let manager = McpManager::new(
        fake.clone(),
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::clock(),
    );
    let servers = vec![
        config("l1", false, false),
        config("l2", false, false),
        config("l3", false, false),
        config("l4", false, false),
        config("l5", false, false),
        config("r1", true, false),
        config("r2", true, false),
        config("always", false, true),
    ];
    manager.start(crate::test_support::workspace(), servers);
    settle().await;
    let started = initializing(&fake);
    let local: Vec<_> = started
        .iter()
        .filter(|name| name.starts_with('l'))
        .collect();
    // 本机的普通服务器只有 3 个在握手；远端的与 alwaysLoad 的不被它们挡住。
    assert_eq!(local.len(), 3, "{started:?}");
    for name in ["r1", "r2", "always"] {
        assert!(started.iter().any(|started| started == name), "{started:?}");
    }
    // 一个握手完成，名额还回去，下一个本机服务器开始握手。
    let first = local[0].clone();
    fake.release(&first, json!({}), None);
    assert_eq!(local_initializing(&fake, 4).await, 4);
    // 只多一个：名额仍是 3（一个连上了，两个在握手，加上新的一个）。
    settle().await;
    assert_eq!(local_initializing(&fake, 5).await, 4);
}

#[tokio::test]
async fn a_reconnect_does_not_wait_for_a_slot() {
    let mut fake = FakeMcp::default();
    for name in ["l1", "l2", "l3"] {
        fake = fake.with(name, held(|_, _| None));
    }
    let fake = Arc::new(fake.with("quick", quiet()));
    let scope = TaskScope::root("mcp-batch", |_| {});
    let mut settings = McpSettings::default();
    settings.batch.local = 3;
    let manager = McpManager::new(
        fake.clone(),
        scope.spawner(),
        settings,
        crate::clock_tests::clock(),
    );
    let servers = ["l1", "l2", "l3", "quick"]
        .into_iter()
        .map(|name| config(name, false, false))
        .collect();
    manager.start(crate::test_support::workspace(), servers);
    settle().await;
    // 三个慢的占满名额，`quick` 在等。
    assert_eq!(initializing(&fake), ["l1", "l2", "l3"]);
    // `/mcp reconnect` 直接连，不等名额（同对照实现）。
    let infos = manager
        .control(McpAction::Reconnect, &["quick".to_owned()])
        .await;
    assert_eq!(infos.len(), 1);
    assert!(initializing(&fake).iter().any(|name| name == "quick"));
}
