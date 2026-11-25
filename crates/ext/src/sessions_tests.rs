//! 会话级的 MCP 连接（ext.md §3.9，工作线 F145）：`PerSession` 时每个顶层会话一份连接，声明 `isolation: "workspace"`
//! 的服务器内核级一份；运行按顶层会话的视图看工具；会话释放后它的那一份断开。`Process` 时会话打开不新连。

use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::testing::{ListTools, MemoryEnvironment, SequentialIds};
use serde_json::json;

use super::*;
use crate::mcp::McpServerOptions;
use crate::test_support::{EchoBash, FakeMcp, facts, server, workspace};

fn fake() -> Arc<FakeMcp> {
    let lister = |tool: &'static str| {
        move |method: &str, _: &serde_json::Value| {
            (method == "tools/list")
                .then(|| Ok(json!({"tools": [{"name": tool, "inputSchema": {"type": "object"}}]})))
        }
    };
    Arc::new(
        FakeMcp::default()
            .with("alpha", server(json!({"tools": {}}), None, lister("probe")))
            .with("beta", server(json!({"tools": {}}), None, lister("probe"))),
    )
}

fn stdio(name: &str, isolation: McpIsolation) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::Project,
        transport: McpTransportConfig::stdio("node".to_owned(), Vec::new()),
        options: McpServerOptions {
            isolation,
            ..Default::default()
        },
        machine: Default::default(),
    }
}

async fn ext(fake: Arc<FakeMcp>, scope: &TaskScope, sharing: McpSharing) -> Ext {
    let base: Arc<dyn ToolRegistry> = Arc::new(ListTools(vec![Arc::new(EchoBash::default())]));
    let env = Arc::new(MemoryEnvironment::new(facts()));
    let ports = ExtPorts {
        info: env.clone(),
        files: env,
        ids: Arc::new(SequentialIds::default()),
        gate: Arc::new(crate::test_support::AllowedGate::default()),
        mcp: fake,
        machines: None,
        spawner: scope.spawner(),
        clock: crate::clock_tests::clock(),
        admission: None,
        sign_ins: None,
        hooks: crate::test_support::no_hooks(),
        model: crate::test_support::no_model(),
        default_model: crate::test_support::default_model(),
        watch: None,
    };
    let config = ExtConfig {
        mcp: McpSettings {
            servers: vec![
                stdio("alpha", McpIsolation::Session),
                stdio("beta", McpIsolation::Workspace),
            ],
            ..McpSettings::default()
        },
        workspace: Some(workspace()),
        mcp_sharing: sharing,
        ..ExtConfig::default()
    };
    let ext = Ext::new(ports, base, config);
    ext.start();
    ext
}

fn connects(fake: &FakeMcp, server: &str) -> usize {
    fake.roots()
        .iter()
        .filter(|(name, _)| name == server)
        .count()
}

fn names(registry: &Arc<dyn ToolRegistry>) -> Vec<String> {
    registry
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

/// 等这个视图里出现这个工具（连接任务是异步的）。
async fn until_listed(registry: &Arc<dyn ToolRegistry>, name: &str) {
    for _ in 0..200 {
        if names(registry).iter().any(|known| known == name) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{name} never appeared: {:?}", names(registry));
}

#[tokio::test]
async fn each_session_owns_its_connections_and_shared_servers_connect_once() {
    let fake = fake();
    let scope = TaskScope::root("test", |_| {});
    let ext = ext(fake.clone(), &scope, McpSharing::PerSession).await;
    let (one, two) = (SessionId::new("sess_1"), SessionId::new("sess_2"));
    ext.session_opened(&one);
    ext.session_opened(&two);
    let view_one = ext.registry().view(&one);
    let view_two = ext.registry().view(&two);
    until_listed(&view_one, "mcp__alpha__probe").await;
    until_listed(&view_two, "mcp__alpha__probe").await;
    until_listed(&view_one, "mcp__beta__probe").await;
    // 会话级的 alpha 每个会话连一次；声明 workspace 的 beta 只连一次（内核级）。
    assert_eq!(connects(&fake, "alpha"), 2);
    assert_eq!(connects(&fake, "beta"), 1);
    // 内核级的注册表（没有顶层会话的视图）只看得到共用的。
    let kernel = names(&ext.registry());
    assert!(
        kernel.contains(&"mcp__beta__probe".to_owned()),
        "{kernel:?}"
    );
    assert!(
        !kernel.contains(&"mcp__alpha__probe".to_owned()),
        "{kernel:?}"
    );
    // 释放会话 1：它的那一份断开，会话 2 的照旧。
    assert_eq!(ext.mcp_sessions().open_sets(), 2);
    ext.session_released(&one);
    assert_eq!(ext.mcp_sessions().open_sets(), 1);
    let after = names(&ext.registry().view(&one));
    assert!(
        !after.contains(&"mcp__alpha__probe".to_owned()),
        "{after:?}"
    );
    assert!(names(&ext.registry().view(&two)).contains(&"mcp__alpha__probe".to_owned()));
}

#[tokio::test]
async fn a_process_wide_set_does_not_connect_per_session() {
    let fake = fake();
    let scope = TaskScope::root("test", |_| {});
    let ext = ext(fake.clone(), &scope, McpSharing::Process).await;
    let one = SessionId::new("sess_1");
    ext.session_opened(&one);
    let view = ext.registry().view(&one);
    until_listed(&view, "mcp__alpha__probe").await;
    until_listed(&view, "mcp__beta__probe").await;
    // `isolation` 不看：两个服务器都在内核级，各连一次；内核级的注册表与视图看到的一样。
    assert_eq!(connects(&fake, "alpha"), 1);
    assert_eq!(connects(&fake, "beta"), 1);
    assert_eq!(ext.mcp_sessions().open_sets(), 0);
    assert_eq!(names(&ext.registry()), names(&view));
}
