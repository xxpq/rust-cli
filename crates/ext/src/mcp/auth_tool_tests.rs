//! 授权伪工具（ext.md §3.12.16，D56）：交互会话里要授权的服务器有两个工具（print 会话没有）；描述与 schema 逐字比对（§3.12.16）；authenticate
//! 开始一次授权、给出地址，180 s 内接着用；complete_authentication 的几种结果；授权完成之后换成真实工具。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::mcp::{McpError, McpTransportConfig};
use rust_cli_kernel_api::mcp_auth::{
    McpAuthTarget, McpAuthorizer, McpPendingSignIn, PastedCallback,
};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::testing::MemoryEnvironment;
use rust_cli_kernel_api::tool::{PromptContext, Tool};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::super::{McpManager, McpServer, McpSignIns};
use crate::mcp::{McpScope, McpSettings};
use crate::test_support::{FakeMcp, facts, server, tool_context, workspace};

const URL: &str = "https://mcp.example.com/mcp";
const AUTHORIZE: &str = "https://auth.example.com/authorize?state=s&redirect_uri=http%3A%2F%2F127.0.0.1%3A4545%2Foauth%2Fcallback%2Fmcp%2Fdocs";

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
        _ => None,
    })
}

/// 授权端口的替身：`finish` 等用户贴回调地址（state 是 `s` 的才收），收下后让假服务器认下这个服务器。
struct Authorizer {
    fake: Arc<FakeMcp>,
    began: Mutex<u32>,
}

struct Pending {
    fake: Arc<FakeMcp>,
    server: String,
}

#[async_trait]
impl McpPendingSignIn for Pending {
    fn url(&self) -> &str {
        AUTHORIZE
    }

    async fn finish(
        self: Box<Self>,
        mut pasted: mpsc::Receiver<PastedCallback>,
    ) -> Result<(), McpError> {
        while let Some(callback) = pasted.recv().await {
            let ours = callback.url.contains("state=s");
            let _ = callback.accepted.send(ours);
            if ours {
                self.fake.authorize(&self.server);
                return Ok(());
            }
        }
        Err(McpError::Connect {
            message: "gone".to_owned(),
        })
    }
}

#[async_trait]
impl McpAuthorizer for Authorizer {
    async fn begin(
        &self,
        target: &McpAuthTarget,
        _scope: Option<&str>,
    ) -> Result<Box<dyn McpPendingSignIn>, McpError> {
        *self.began.lock().unwrap() += 1;
        Ok(Box::new(Pending {
            fake: self.fake.clone(),
            server: target.server.clone(),
        }))
    }

    async fn sign_out(&self, _target: &McpAuthTarget) -> Result<(), McpError> {
        Ok(())
    }

    async fn signed_in(&self, _target: &McpAuthTarget) -> bool {
        false
    }
}

/// 一个要授权的 `docs`；`print` 是 `-p` 的会话。
async fn needing_auth(
    scope: &TaskScope,
    print: bool,
) -> (McpManager, Arc<FakeMcp>, Arc<Authorizer>) {
    let fake = Arc::new(FakeMcp::default().with("docs", docs_server()));
    fake.require_auth("docs", None);
    let manager = McpManager::new(
        fake.clone(),
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::clock(),
    );
    manager.set_print_session(print);
    let authorizer = Arc::new(Authorizer {
        fake: fake.clone(),
        began: Mutex::new(0),
    });
    manager.set_sign_ins(Arc::new(McpSignIns::new(
        authorizer.clone(),
        crate::clock_tests::clock(),
    )));
    manager.start(workspace(), vec![remote("docs")]);
    manager.wait_ready().await;
    (manager, fake, authorizer)
}

fn names(manager: &McpManager) -> Vec<String> {
    manager
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

async fn call(tool: &Arc<dyn Tool>, input: Value) -> String {
    let (progress, _) = mpsc::channel(4);
    let output = tool
        .call(
            input,
            tool_context(
                Arc::new(MemoryEnvironment::new(facts())),
                PermissionMode::Default,
            ),
            progress,
        )
        .await
        .unwrap();
    match &output.content[0] {
        rust_cli_kernel_api::message::ToolResultContent::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_server_needing_auth_has_the_two_stubs_in_interactive_sessions_only() {
    let scope = TaskScope::root("test", |_| {});
    let (manager, _, _) = needing_auth(&scope, false).await;
    assert_eq!(
        names(&manager),
        [
            "mcp__docs__authenticate",
            "mcp__docs__complete_authentication"
        ]
    );
    // 伪工具不算服务器的工具
    assert_eq!(manager.server_infos()[0].tools, 0);
    let authenticate = manager.find("mcp__docs__authenticate").unwrap();
    assert_eq!(
        authenticate.description(&PromptContext {
            tool_names: Vec::new(),
            mode: PermissionMode::Default,
        }),
        "The \"docs\" MCP server (http at https://mcp.example.com) is installed but requires authentication. Call this tool to start the OAuth flow \u{2014} you'll receive an authorization URL to share with the user. Once the user completes authorization in their browser, the server's real tools will become available automatically."
    );
    assert_eq!(
        authenticate.input_schema(),
        json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "properties": {}, "additionalProperties": false})
    );
    assert!(
        authenticate
            .mcp_info()
            .is_some_and(|info| !info.always_load)
    );
    assert!(matches!(
        authenticate
            .check_permissions(
                &json!({}),
                &tool_context(
                    Arc::new(MemoryEnvironment::new(facts())),
                    PermissionMode::Default
                )
            )
            .await,
        PermissionDecision::Allow { .. }
    ));
    let complete = manager.find("mcp__docs__complete_authentication").unwrap();
    assert_eq!(complete.input_schema()["required"], json!(["callback_url"]));

    let scope = TaskScope::root("test", |_| {});
    let (print, _, _) = needing_auth(&scope, true).await;
    assert!(names(&print).is_empty());
}

#[tokio::test]
async fn authenticate_gives_the_url_reuses_its_own_flow_and_complete_finishes_it() {
    let scope = TaskScope::root("test", |_| {});
    let (manager, _, authorizer) = needing_auth(&scope, false).await;
    let authenticate = manager.find("mcp__docs__authenticate").unwrap();
    let complete = manager.find("mcp__docs__complete_authentication").unwrap();

    assert_eq!(
        call(
            &complete,
            json!({"callback_url": "http://127.0.0.1:4545/cb?code=c&state=s"})
        )
        .await,
        "No OAuth flow is in progress for docs. Call `mcp__docs__authenticate` first, then retry with the callback URL."
    );
    let first = call(&authenticate, json!({})).await;
    assert_eq!(
        first,
        format!(
            "Ask the user to open this URL in their browser to authorize the docs MCP server:\n\n{AUTHORIZE}\n\nOnce they complete the flow, the server's tools will become available automatically.\n\nIf the browser shows a connection error on the redirect page, ask the user to paste the full URL from the address bar and call `mcp__docs__complete_authentication` with it."
        )
    );
    let again = call(&authenticate, json!({})).await;
    assert!(
        again.starts_with("A sign-in for docs that started earlier is still in progress, so the link below is that sign-in's link, not a new one."),
        "{again}"
    );
    assert_eq!(*authorizer.began.lock().unwrap(), 1);

    assert!(
        call(
            &complete,
            json!({"callback_url": "http://127.0.0.1:4545/cb?state=s"})
        )
        .await
        .starts_with("Invalid callback URL: missing authorization code.")
    );
    assert!(
        call(
            &complete,
            json!({"callback_url": "http://127.0.0.1:4545/cb?code=c&state=other"})
        )
        .await
        .starts_with("That callback URL belongs to a different sign-in attempt for docs")
    );
    assert_eq!(
        call(
            &complete,
            json!({"callback_url": "http://127.0.0.1:4545/cb?code=c&state=s"})
        )
        .await,
        "Authentication complete for docs. The server's tools should now be available."
    );
    // 重连之后伪工具换成真实工具
    assert_eq!(names(&manager), ["mcp__docs__search"]);
}
