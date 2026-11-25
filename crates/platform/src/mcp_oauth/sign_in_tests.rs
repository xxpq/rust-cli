//! 交互授权的 scope 与回调路径（TS SDK `determineScope`、Node `normalizeCallbackPath`）。

use rust_cli_kernel_api::mcp_auth::McpOAuth;
use serde_json::json;

use super::*;

fn target(scope: Option<&str>, redirect_path: Option<&str>) -> McpAuthTarget {
    McpAuthTarget {
        server: "plugin:kit:canva".to_owned(),
        url: "https://mcp.example.com/mcp".to_owned(),
        oauth: McpOAuth {
            scope: scope.map(str::to_owned),
            redirect_path: redirect_path.map(str::to_owned),
            ..McpOAuth::default()
        },
    }
}

fn discovery(resource_scopes: &[&str], server_scopes: &[&str]) -> Discovery {
    Discovery {
        authorization_server_url: "https://auth.example.com".to_owned(),
        resource_metadata_url: None,
        resource_metadata: Some(
            json!({"resource": "https://mcp.example.com/mcp", "scopes_supported": resource_scopes}),
        ),
        authorization_server_metadata: Some(
            json!({"issuer": "https://auth.example.com", "scopes_supported": server_scopes}),
        ),
    }
}

#[test]
fn the_scope_is_the_configured_one_then_the_resource_s_and_gets_offline_access_when_offered() {
    let found = discovery(
        &["files:read", "files:write"],
        &["files:read", "offline_access"],
    );
    assert_eq!(
        scope_of(&target(Some("mcp:connect"), None), &found, None, None).as_deref(),
        Some("mcp:connect offline_access")
    );
    assert_eq!(
        scope_of(&target(None, None), &found, None, None).as_deref(),
        Some("files:read files:write offline_access")
    );
    // 授权服务器不提供 offline_access：不加
    let plain = discovery(&["files:read"], &["files:read"]);
    assert_eq!(
        scope_of(&target(None, None), &plain, None, None).as_deref(),
        Some("files:read")
    );
    // 没有任何来源：不带 scope
    let none = discovery(&[], &["offline_access"]);
    assert_eq!(scope_of(&target(None, None), &none, None, None), None);
}

#[test]
fn a_step_up_asks_for_the_union_of_configured_granted_and_required_scopes() {
    let found = discovery(&["files:read"], &[]);
    assert_eq!(
        scope_of(&target(Some("a b"), None), &found, Some("b c"), Some("c d")).as_deref(),
        Some("a b c d")
    );
}

#[test]
fn the_callback_path_is_the_configured_one_or_node_s_default_with_the_encoded_name() {
    assert_eq!(
        callback_path(&target(None, None)),
        "/oauth/callback/mcp/plugin%3Akit%3Acanva"
    );
    assert_eq!(callback_path(&target(None, Some("/cb"))), "/cb");
    assert_eq!(callback_path(&target(None, Some("cb"))), "/cb");
}
