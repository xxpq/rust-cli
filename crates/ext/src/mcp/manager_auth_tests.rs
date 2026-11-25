//! 远程服务器的 OAuth 在连接管理里（ext.md §3.12）：连接时要授权 → NeedsAuth、不重连；工具调用时要授权 → 调用失败（要用户
//! 重新授权的文字）、服务器转成 NeedsAuth；Sign in 完成后重连；Sign out 之后要授权；`/mcp` 的快照带有没有令牌；没有授权端口时不能 Sign in。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::manage::{McpAuthState, McpServerState};
use rust_cli_kernel_api::mcp::McpError;
use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpAuthorizer, McpPendingSignIn};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::tool::ToolError;
use serde_json::json;
use tokio::sync::mpsc;

use super::super::{McpManager, McpServer, McpSignIns, McpStatus};
use crate::mcp::{McpScope, McpSessions, McpSettings, McpSharing};
use crate::test_support::{FakeMcp, facts, server, tool_context, workspace};
use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::testing::MemoryEnvironment;

const URL: &str = "https://mcp.example.com/mcp";

fn remote(name: &str) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::User,
        transport: McpTransportConfig::http(URL.to_owned(), Default::default()),
        options: Default::default(),
        machine: Default::default(),
    }
}

fn docs_server() -> Arc<crate::test_support::Handler> {
    server(json!({"tools": {}}), None, |method, _| match method {
        "tools/list" => Some(Ok(
            json!({"tools": [{"name": "search", "inputSchema": {"type": "object"}}]}),
        )),
        "tools/call" => Some(Ok(json!({"content": [{"type": "text", "text": "found"}]}))),
        _ => None,
    })
}

/// 授权端口的替身：`begin` 记下目标与 step-up 的 scope；`finish` 让假服务器认下这个服务器（令牌存下了）。
struct Authorizer {
    fake: Arc<FakeMcp>,
    signed_in: Mutex<bool>,
    began: Mutex<Vec<(McpAuthTarget, Option<String>)>>,
}

struct Pending {
    fake: Arc<FakeMcp>,
    server: String,
}

#[async_trait]
impl McpPendingSignIn for Pending {
    fn url(&self) -> &str {
        "https://auth.example.com/authorize?state=s"
    }

    async fn finish(
        self: Box<Self>,
        _pasted: tokio::sync::mpsc::Receiver<rust_cli_kernel_api::mcp_auth::PastedCallback>,
    ) -> Result<(), McpError> {
        self.fake.authorize(&self.server);
        Ok(())
    }
}

#[async_trait]
impl McpAuthorizer for Authorizer {
    async fn begin(
        &self,
        target: &McpAuthTarget,
        scope: Option<&str>,
    ) -> Result<Box<dyn McpPendingSignIn>, McpError> {
        self.began
            .lock()
            .unwrap()
            .push((target.clone(), scope.map(str::to_owned)));
        *self.signed_in.lock().unwrap() = true;
        Ok(Box::new(Pending {
            fake: self.fake.clone(),
            server: target.server.clone(),
        }))
    }

    async fn sign_out(&self, target: &McpAuthTarget) -> Result<(), McpError> {
        *self.signed_in.lock().unwrap() = false;
        self.fake.require_auth(&target.server, None);
        Ok(())
    }

    async fn signed_in(&self, _target: &McpAuthTarget) -> bool {
        *self.signed_in.lock().unwrap()
    }
}

/// 内核级的管理器与它的会话表（TUI 的 `Process`：全部服务器在内核级；Sign in 与 Sign out 经会话表，ext.md §3.12.9）。
fn setup(
    scope: &TaskScope,
    authorizer: bool,
) -> (
    Arc<McpManager>,
    Arc<McpSessions>,
    Arc<FakeMcp>,
    Arc<Authorizer>,
) {
    let fake = Arc::new(FakeMcp::default().with("docs", docs_server()));
    let manager = Arc::new(McpManager::new(
        fake.clone(),
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::clock(),
    ));
    let port = Arc::new(Authorizer {
        fake: fake.clone(),
        signed_in: Mutex::new(false),
        began: Mutex::default(),
    });
    if authorizer {
        manager.set_sign_ins(Arc::new(McpSignIns::new(
            port.clone(),
            crate::clock_tests::clock(),
        )));
    }
    let sessions = Arc::new(McpSessions::new(
        manager.clone(),
        McpSharing::Process,
        scope.spawner(),
    ));
    (manager, sessions, fake, port)
}

#[tokio::test]
async fn a_server_that_needs_authorization_waits_for_sign_in_and_connects_after_it() {
    let scope = TaskScope::root("test", |_| {});
    let (manager, sessions, fake, authorizer) = setup(&scope, true);
    fake.require_auth("docs", None);
    manager.start(workspace(), vec![remote("docs")]);
    manager.wait_ready().await;
    assert_eq!(
        manager.status(),
        [("docs".to_owned(), McpStatus::NeedsAuth { scope: None })]
    );
    // 不退避重连：一次 initialize
    let initializes = || {
        fake.received()
            .iter()
            .filter(|(_, message)| message["method"] == "initialize")
            .count()
    };
    assert_eq!(initializes(), 1);
    let infos = manager.server_infos_with_auth().await;
    assert_eq!(infos[0].state, McpServerState::NeedsAuth);
    assert_eq!(infos[0].auth, Some(McpAuthState::SignedOut));

    let sign_in = sessions.sign_in("docs").await.unwrap();
    assert_eq!(sign_in.url, "https://auth.example.com/authorize?state=s");
    let after = sign_in.done.await.unwrap().unwrap();
    assert_eq!(after.state, McpServerState::Connected);
    assert_eq!(after.tools, 1);
    assert_eq!(after.auth, Some(McpAuthState::SignedIn));
    let (target, step_up) = authorizer.began.lock().unwrap()[0].clone();
    assert_eq!(
        (target.server.as_str(), target.url.as_str(), step_up),
        ("docs", URL, None)
    );
}

#[tokio::test]
async fn a_tool_call_that_needs_authorization_fails_with_a_sign_in_hint_and_the_server_waits_for_sign_in()
 {
    let scope = TaskScope::root("test", |_| {});
    let (manager, sessions, fake, authorizer) = setup(&scope, true);
    manager.start(workspace(), vec![remote("docs")]);
    manager.wait_ready().await;
    let tool = manager
        .tools()
        .into_iter()
        .find(|tool| tool.name() == "mcp__docs__search")
        .unwrap();
    // 令牌被吊销、要更多权限：传输报 AuthRequired 带它要的 scope
    fake.require_auth("docs", Some("files:write"));
    let (progress, _) = mpsc::channel(4);
    let error = tool
        .call(
            json!({}),
            tool_context(
                Arc::new(MemoryEnvironment::new(facts())),
                PermissionMode::Default,
            ),
            progress,
        )
        .await
        .unwrap_err();
    let ToolError::Failed { message: text } = error else {
        panic!("{error:?}");
    };
    assert_eq!(
        text,
        "MCP server \"docs\" needs additional permissions (scope: \"files:write\") — run /mcp to re-authenticate"
    );
    assert_eq!(
        manager.status(),
        [(
            "docs".to_owned(),
            McpStatus::NeedsAuth {
                scope: Some("files:write".to_owned())
            }
        )]
    );
    assert!(
        manager
            .tools()
            .iter()
            .all(|tool| tool.name() != "mcp__docs__search")
    );
    // 下次 Sign in 并进要的 scope
    let sign_in = sessions.sign_in("docs").await.unwrap();
    assert_eq!(
        authorizer.began.lock().unwrap()[0].1.as_deref(),
        Some("files:write")
    );
    assert_eq!(
        sign_in.done.await.unwrap().unwrap().state,
        McpServerState::Connected
    );
}

#[tokio::test]
async fn sign_out_forgets_the_tokens_and_the_server_then_needs_authorization() {
    let scope = TaskScope::root("test", |_| {});
    let (manager, sessions, _fake, authorizer) = setup(&scope, true);
    *authorizer.signed_in.lock().unwrap() = true;
    manager.start(workspace(), vec![remote("docs")]);
    manager.wait_ready().await;
    let after = sessions.sign_out("docs").await.unwrap().unwrap();
    assert_eq!(after.state, McpServerState::NeedsAuth);
    assert_eq!(after.auth, Some(McpAuthState::SignedOut));
}

#[tokio::test]
async fn without_an_authorizer_or_oauth_there_is_no_sign_in() {
    let scope = TaskScope::root("test", |_| {});
    let (manager, sessions, fake, _) = setup(&scope, false);
    fake.require_auth("docs", None);
    manager.start(workspace(), vec![remote("docs")]);
    manager.wait_ready().await;
    // 仍报要授权，只是不能在这里 Sign in
    assert_eq!(
        manager.status(),
        [("docs".to_owned(), McpStatus::NeedsAuth { scope: None })]
    );
    assert_eq!(manager.server_infos_with_auth().await[0].auth, None);
    assert!(sessions.sign_in("docs").await.is_err());

    // 配置了 Authorization 头：不用 OAuth，没有 Sign in
    let (manager, sessions, _, _) = setup(&scope, true);
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("Authorization".to_owned(), "Bearer static".to_owned());
    let static_auth = McpServer {
        transport: McpTransportConfig::http(URL.to_owned(), headers),
        ..remote("docs")
    };
    manager.start(workspace(), vec![static_auth]);
    manager.wait_ready().await;
    assert_eq!(manager.server_infos_with_auth().await[0].auth, None);
    assert!(sessions.sign_in("docs").await.is_err());
}
