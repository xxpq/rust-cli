//! 进程级的授权登记（ext.md §3.12.14）：一次授权之后，这个进程里所有管理器（多套内核、会话级的兄弟）同一授权目标的、要授权的连接都重连；
//! 别的目标不动；代号变了时连接直接再连一次；用户再发起替换旧的；退出授权之后连着的进 NeedsAuth。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::manage::McpSignInOutcome;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, McpTransportConfig, McpTransportEvent,
};
use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpAuthorizer, McpPendingSignIn};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use super::super::{McpManager, McpServer, McpStatus};
use super::{McpSignIns, settled};
use crate::mcp::{McpScope, McpSettings};
use crate::test_support::{FakeMcp, server, workspace};

const URL: &str = "https://mcp.example.com/mcp";
const OTHER_URL: &str = "https://other.example.com/mcp";

fn remote(name: &str, url: &str) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::User,
        transport: McpTransportConfig::http(url.to_owned(), Default::default()),
        options: Default::default(),
        machine: Default::default(),
    }
}

fn tools_server() -> Arc<crate::test_support::Handler> {
    server(json!({"tools": {}}), None, |method, _| match method {
        "tools/list" => Some(Ok(
            json!({"tools": [{"name": "search", "inputSchema": {"type": "object"}}]}),
        )),
        _ => None,
    })
}

/// 授权端口的替身：`finish` 等测试放行（`gate`），然后让假服务器认下这个服务器（令牌存下了）。
struct Authorizer {
    fake: Arc<FakeMcp>,
    began: Mutex<u32>,
    gate: Arc<Notify>,
    hold: bool,
}

struct Pending {
    fake: Arc<FakeMcp>,
    server: String,
    url: String,
    gate: Option<Arc<Notify>>,
}

#[async_trait]
impl McpPendingSignIn for Pending {
    fn url(&self) -> &str {
        &self.url
    }

    async fn finish(
        self: Box<Self>,
        _pasted: tokio::sync::mpsc::Receiver<rust_cli_kernel_api::mcp_auth::PastedCallback>,
    ) -> Result<(), McpError> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.fake.authorize(&self.server);
        Ok(())
    }
}

#[async_trait]
impl McpAuthorizer for Authorizer {
    async fn begin(
        &self,
        target: &McpAuthTarget,
        _scope: Option<&str>,
    ) -> Result<Box<dyn McpPendingSignIn>, McpError> {
        let mut began = self.began.lock().unwrap();
        *began += 1;
        Ok(Box::new(Pending {
            fake: self.fake.clone(),
            server: target.server.clone(),
            url: format!("https://auth.example.com/authorize?state={began}"),
            gate: self.hold.then(|| self.gate.clone()),
        }))
    }

    async fn sign_out(&self, target: &McpAuthTarget) -> Result<(), McpError> {
        self.fake.require_auth(&target.server, None);
        Ok(())
    }

    async fn signed_in(&self, _target: &McpAuthTarget) -> bool {
        false
    }
}

fn manager(
    scope: &TaskScope,
    transport: Arc<dyn McpTransport>,
    sign_ins: &Arc<McpSignIns>,
) -> McpManager {
    let manager = McpManager::new(
        transport,
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::clock(),
    );
    manager.set_sign_ins(sign_ins.clone());
    manager
}

fn registry(fake: &Arc<FakeMcp>, hold: bool) -> (Arc<McpSignIns>, Arc<Authorizer>) {
    let authorizer = Arc::new(Authorizer {
        fake: fake.clone(),
        began: Mutex::new(0),
        gate: Arc::new(Notify::new()),
        hold,
    });
    (
        Arc::new(McpSignIns::new(
            authorizer.clone(),
            crate::clock_tests::clock(),
        )),
        authorizer,
    )
}

fn target(name: &str, url: &str) -> McpAuthTarget {
    remote(name, url).transport.auth_target(name).unwrap()
}

fn status_of(manager: &McpManager, name: &str) -> McpStatus {
    manager
        .status()
        .into_iter()
        .find(|(known, _)| known == name)
        .unwrap()
        .1
}

/// 两套内核（App 的两个工作区）里同一个服务器都要授权：在一套里授权，两套都连上；别的授权目标不动。
#[tokio::test]
async fn one_sign_in_reconnects_the_target_in_every_kernel_and_leaves_other_targets() {
    let scope = TaskScope::root("test", |_| {});
    let fake = Arc::new(
        FakeMcp::default()
            .with("docs", tools_server())
            .with("other", tools_server()),
    );
    fake.require_auth("docs", None);
    fake.require_auth("other", None);
    let (sign_ins, _) = registry(&fake, false);
    let first = manager(&scope, fake.clone(), &sign_ins);
    let second = manager(&scope, fake.clone(), &sign_ins);
    first.start(
        workspace(),
        vec![remote("docs", URL), remote("other", OTHER_URL)],
    );
    second.start(workspace(), vec![remote("docs", URL)]);
    first.wait_ready().await;
    second.wait_ready().await;
    assert_eq!(
        status_of(&second, "docs"),
        McpStatus::NeedsAuth { scope: None }
    );

    let started = sign_ins
        .begin(target("docs", URL), None, false, &scope.spawner())
        .await
        .unwrap();
    assert_eq!(settled(started.outcome).await, McpSignInOutcome::SignedIn);
    assert!(status_of(&first, "docs").connected());
    assert!(status_of(&second, "docs").connected());
    // 别的目标还是要授权（没有被重连：它的 initialize 只有一次）
    assert_eq!(
        status_of(&first, "other"),
        McpStatus::NeedsAuth { scope: None }
    );
    let others = fake
        .received()
        .iter()
        .filter(|(name, message)| name == "other" && message["method"] == "initialize")
        .count();
    assert_eq!(others, 1);
}

/// 会话级的兄弟（App 的每个会话一份）也登记：授权之后一起重连；释放了的跳过。
#[tokio::test]
async fn siblings_are_reconnected_and_dropped_managers_are_skipped() {
    let scope = TaskScope::root("test", |_| {});
    let fake = Arc::new(FakeMcp::default().with("docs", tools_server()));
    fake.require_auth("docs", None);
    let (sign_ins, _) = registry(&fake, false);
    let kernel = manager(&scope, fake.clone(), &sign_ins);
    let session = kernel.sibling();
    let released = kernel.sibling();
    session.start(workspace(), vec![remote("docs", URL)]);
    released.start(workspace(), vec![remote("docs", URL)]);
    session.wait_ready().await;
    released.wait_ready().await;
    released.shutdown().await;
    drop(released);

    let started = sign_ins
        .begin(target("docs", URL), None, false, &scope.spawner())
        .await
        .unwrap();
    assert_eq!(settled(started.outcome).await, McpSignInOutcome::SignedIn);
    assert!(status_of(&session, "docs").connected());
}

/// 用户再发起一次：丢下进行中的那一次（结局是被替换）；地址按目标查得到最新的那一次。
#[tokio::test]
async fn a_new_sign_in_replaces_the_one_in_progress() {
    let scope = TaskScope::root("test", |_| {});
    let fake = Arc::new(FakeMcp::default().with("docs", tools_server()));
    let (sign_ins, authorizer) = registry(&fake, true);
    let docs = target("docs", URL);
    let first = sign_ins
        .begin(docs.clone(), None, false, &scope.spawner())
        .await
        .unwrap();
    let second = sign_ins
        .begin(docs.clone(), None, false, &scope.spawner())
        .await
        .unwrap();
    assert_eq!(settled(first.outcome).await, McpSignInOutcome::Replaced);
    assert_eq!(sign_ins.pending_url(&docs), Some(second.url.clone()));
    assert!(sign_ins.outcome_of(&docs, &first.url).is_none());
    authorizer.gate.notify_one();
    assert_eq!(settled(second.outcome).await, McpSignInOutcome::SignedIn);
    assert_eq!(sign_ins.pending_url(&docs), None);
}

/// 退出授权：这个目标连着的连接重连，之后要授权。
#[tokio::test]
async fn signing_out_reconnects_into_needs_auth() {
    let scope = TaskScope::root("test", |_| {});
    let fake = Arc::new(FakeMcp::default().with("docs", tools_server()));
    let (sign_ins, _) = registry(&fake, false);
    let kernel = manager(&scope, fake.clone(), &sign_ins);
    kernel.start(workspace(), vec![remote("docs", URL)]);
    kernel.wait_ready().await;
    assert!(status_of(&kernel, "docs").connected());
    sign_ins.sign_out(&target("docs", URL)).await.unwrap();
    assert_eq!(
        status_of(&kernel, "docs"),
        McpStatus::NeedsAuth { scope: None }
    );
}

/// 在授权完成的那一刻正在连的连接（ext.md §3.12.14 的竞态表第 2 行）：它的握手在发布之前失败、状态在通知之后才写。
/// 代号变了，所以它直接再连一次、连上，不停在要授权。旧代码上停在 NeedsAuth。
struct Racing {
    fake: Arc<FakeMcp>,
    /// 第一次 docs 的 initialize：先让测试做完一次授权（经这个口），再报要授权。
    first: Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>>,
}

#[async_trait]
impl McpTransport for Racing {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        self.fake.connect(workspace, request).await
    }

    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError> {
        if message["method"] == "initialize" {
            let armed = self.first.lock().unwrap().take();
            if let Some(armed) = armed {
                let (done, signed_in) = oneshot::channel();
                let _ = armed.send(done);
                let _ = signed_in.await;
                return Err(McpError::AuthRequired {
                    message: "the MCP server needs authorization (HTTP 401)".to_owned(),
                    scope: None,
                });
            }
        }
        self.fake.send(connection, message).await
    }

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        self.fake.events(connection)
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        self.fake.close(connection).await
    }
}

#[tokio::test]
async fn a_connection_that_lost_the_race_with_a_sign_in_connects_again() {
    let scope = TaskScope::root("test", |_| {});
    let fake = Arc::new(FakeMcp::default().with("docs", tools_server()));
    let (sign_ins, _) = registry(&fake, false);
    let (armed, first) = oneshot::channel();
    let racing = Arc::new(Racing {
        fake: fake.clone(),
        first: Mutex::new(Some(armed)),
    });
    let kernel = manager(&scope, racing, &sign_ins);
    kernel.start(workspace(), vec![remote("docs", URL)]);
    // 连接在握手里：这时授权完成（发布、通知；它还是 Pending，没被重连）
    let release = first.await.unwrap();
    let started = sign_ins
        .begin(target("docs", URL), None, false, &scope.spawner())
        .await
        .unwrap();
    assert_eq!(settled(started.outcome).await, McpSignInOutcome::SignedIn);
    // 再让它的握手报要授权
    let _ = release.send(());
    kernel.wait_ready().await;
    assert!(status_of(&kernel, "docs").connected());
}
