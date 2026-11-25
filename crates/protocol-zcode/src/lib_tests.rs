//! 存储的启动状态：checking → ready，同一个 attempt 与数据库 ID，序号递增。
use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::testing::FixedClock;
use rust_cli_protocol_core::connection::{self, Options};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, duplex};

use super::*;

#[tokio::test]
async fn the_storage_state_goes_from_checking_to_ready() {
    let scope = TaskScope::root("test", |_| {});
    let (out, host_in) = duplex(1 << 16);
    let (peer, _writer) = connection::start(out, &scope.spawner(), Options::default());
    let context = Arc::new(Context::new(peer, Arc::new(FixedClock(5))));
    let startup = Startup::new(context, "session-db-1");
    startup.checking().await;
    startup.ready().await;
    let mut lines = BufReader::new(host_in).lines();
    let mut next = async || -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let checking = next().await;
    let ready = next().await;
    assert_eq!(checking["method"], STORAGE_STATE);
    assert_eq!(checking["params"]["phase"], "checking");
    assert_eq!(ready["params"]["phase"], "ready");
    assert_eq!(
        checking["params"]["attemptId"],
        ready["params"]["attemptId"]
    );
    assert_eq!(ready["params"]["databaseId"], "session-db-1");
    assert_eq!(ready["params"]["databaseKind"], "session");
    assert_eq!(checking["params"]["sequence"], 1);
    assert_eq!(ready["params"]["sequence"], 2);
    assert_eq!(ready["params"]["schemaVersion"], 1);
}

/// 路由表（app-protocol.md §15.1）：一个会话只归一个工作区，重复登记同一个工作区没事，换工作区拒绝且不改；Host 的位
/// 只升不降。
#[tokio::test]
async fn a_session_is_routed_to_one_workspace() {
    let scope = TaskScope::root("test", |_| {});
    let (out, _host_in) = duplex(1 << 16);
    let (peer, _writer) = connection::start(out, &scope.spawner(), Options::default());
    let context = Context::new(peer, Arc::new(FixedClock(5)));
    assert_eq!(context.workspace_of("s1"), None);
    context.route("s1", "/w/a").unwrap();
    context.route("s1", "/w/a").unwrap();
    assert_eq!(
        context.route("s1", "/w/b"),
        Err(context::RouteConflict("/w/a".to_owned()))
    );
    assert_eq!(context.workspace_of("s1").as_deref(), Some("/w/a"));
    assert!(!context.host_routes());
    context.set_host_routes();
    assert!(context.host_routes());
}
