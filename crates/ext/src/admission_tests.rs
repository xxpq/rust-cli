//! 工作区声明的 MCP 准入（docs/specs/rust-cli/ext.md §3.1 与 18-mcp-trust.md §12.4）：项目级服务器连接之前问注入的端口，没准入的不连、
//! 状态 `Untrusted`；用户级不问；本进程写下信任之后 `readmit` 只重启 `Untrusted` 的那个；会话级的在下一个会话打开时按新结论连。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::TaskScope;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::manage::{McpServerInfo, McpServerState};
use rust_cli_kernel_api::mcp::{McpAdmission, McpTransportConfig};
use rust_cli_kernel_api::testing::{ListTools, MemoryEnvironment, SequentialIds};
use serde_json::json;

use super::*;
use crate::mcp::McpServerOptions;
use crate::test_support::{EchoBash, FakeMcp, facts, server, workspace};

/// 准入端口的替身：名单里的服务器准入，记下问过的名字。
#[derive(Default)]
struct Trusted {
    names: Mutex<HashSet<String>>,
    asked: Mutex<Vec<String>>,
}

impl Trusted {
    fn trust(&self, name: &str) {
        self.names.lock_unpoisoned().insert(name.to_owned());
    }
}

#[async_trait]
impl McpAdmission for Trusted {
    async fn admits(&self, server: &str) -> bool {
        self.asked.lock_unpoisoned().push(server.to_owned());
        self.names.lock_unpoisoned().contains(server)
    }
}

fn declared(name: &str, scope: McpScope, isolation: McpIsolation) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope,
        transport: McpTransportConfig::stdio("node".to_owned(), Vec::new()),
        options: McpServerOptions {
            isolation,
            ..Default::default()
        },
        machine: Default::default(),
    }
}

fn fake() -> Arc<FakeMcp> {
    let lister = |method: &str, _: &serde_json::Value| {
        (method == "tools/list")
            .then(|| Ok(json!({"tools": [{"name": "probe", "inputSchema": {"type": "object"}}]})))
    };
    Arc::new(
        FakeMcp::default()
            .with("session", server(json!({"tools": {}}), None, lister))
            .with("shared", server(json!({"tools": {}}), None, lister))
            .with("user", server(json!({"tools": {}}), None, lister)),
    )
}

async fn ext(fake: Arc<FakeMcp>, scope: &TaskScope, trusted: Arc<Trusted>) -> Ext {
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
        admission: Some(trusted),
        sign_ins: None,
        hooks: crate::test_support::no_hooks(),
        model: crate::test_support::no_model(),
        default_model: crate::test_support::default_model(),
        watch: None,
    };
    let config = ExtConfig {
        mcp: McpSettings {
            servers: vec![
                declared("session", McpScope::Project, McpIsolation::Session),
                declared("shared", McpScope::Project, McpIsolation::Workspace),
                declared("user", McpScope::User, McpIsolation::Workspace),
            ],
            ..McpSettings::default()
        },
        workspace: Some(workspace()),
        mcp_sharing: McpSharing::PerSession,
        ..ExtConfig::default()
    };
    let ext = Ext::new(ports, base, config);
    ext.start();
    ext
}

fn connects(fake: &FakeMcp, name: &str) -> usize {
    fake.roots()
        .iter()
        .filter(|(server, _)| server == name)
        .count()
}

/// 这个顶层会话（None 是内核级）看到的这个服务器的状态。
fn state(ext: &Ext, root: Option<&SessionId>, name: &str) -> Option<McpServerState> {
    let set = match root {
        Some(root) => ext.mcp_sessions().set(root),
        None => ext.mcp_sessions().kernel_set(),
    };
    let (_, view) = set.snapshot();
    view.servers
        .into_iter()
        .find(|server| server.name == name)
        .map(|server| server.state)
}

/// 等这个服务器离开 connecting（连接任务是异步的）。
async fn settled(ext: &Ext, root: Option<&SessionId>, name: &str) -> McpServerState {
    for _ in 0..200 {
        if let Some(found) = state(ext, root, name)
            && found != McpServerState::Pending
        {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{name} never settled: {:?}", state(ext, root, name));
}

fn kernel_info(ext: &Ext, name: &str) -> McpServerInfo {
    ext.mcp()
        .server_infos()
        .into_iter()
        .find(|info| info.name == name)
        .unwrap()
}

#[tokio::test]
async fn project_servers_without_trust_are_not_connected() {
    let fake = fake();
    let scope = TaskScope::root("test", |_| {});
    let trusted = Arc::new(Trusted::default());
    let ext = ext(fake.clone(), &scope, trusted.clone()).await;
    let one = SessionId::new("sess_1");
    ext.session_opened(&one);
    assert_eq!(settled(&ext, None, "user").await, McpServerState::Connected);
    // 没准入：不连，界面按“没连上”报，原因写明没信任；用户级的不问
    assert_eq!(settled(&ext, None, "shared").await, McpServerState::Failed);
    assert_eq!(
        settled(&ext, Some(&one), "session").await,
        McpServerState::Failed
    );
    let shared = kernel_info(&ext, "shared");
    assert!(
        shared
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("not trusted"),
        "{shared:?}"
    );
    assert_eq!(connects(&fake, "shared"), 0);
    assert_eq!(connects(&fake, "session"), 0);
    let mut asked = trusted.asked.lock_unpoisoned().clone();
    asked.sort();
    assert_eq!(asked, ["session", "shared"]);
}

#[tokio::test]
async fn trust_reaches_the_kernel_by_readmit_and_sessions_on_their_next_open() {
    let fake = fake();
    let scope = TaskScope::root("test", |_| {});
    let trusted = Arc::new(Trusted::default());
    let ext = ext(fake.clone(), &scope, trusted.clone()).await;
    let one = SessionId::new("sess_1");
    ext.session_opened(&one);
    settled(&ext, None, "shared").await;
    settled(&ext, Some(&one), "session").await;
    trusted.trust("shared");
    trusted.trust("session");
    // 内核级的：本进程写下信任之后补连一次
    ext.mcp().readmit("shared").await;
    assert_eq!(
        settled(&ext, None, "shared").await,
        McpServerState::Connected
    );
    assert_eq!(connects(&fake, "shared"), 1);
    // 连着的不动
    ext.mcp().readmit("shared").await;
    assert_eq!(connects(&fake, "shared"), 1);
    // 会话级的不重放：已打开的会话照旧，下一个会话按新结论连
    assert_eq!(
        state(&ext, Some(&one), "session"),
        Some(McpServerState::Failed)
    );
    let two = SessionId::new("sess_2");
    ext.session_opened(&two);
    assert_eq!(
        settled(&ext, Some(&two), "session").await,
        McpServerState::Connected
    );
    assert_eq!(connects(&fake, "session"), 1);
}
