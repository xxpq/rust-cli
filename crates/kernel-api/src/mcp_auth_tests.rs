use std::collections::BTreeMap;

use super::*;
use crate::mcp::McpTransportConfig;

fn headers(names: &[&str]) -> BTreeMap<String, String> {
    names
        .iter()
        .map(|name| ((*name).to_owned(), "value".to_owned()))
        .collect()
}

#[test]
fn http_and_sse_servers_use_oauth_unless_an_authorization_header_is_configured() {
    let plain = McpTransportConfig::http("https://mcp.example.com/mcp", headers(&["X-Api"]));
    assert_eq!(
        plain.auth_target("docs"),
        Some(McpAuthTarget {
            server: "docs".to_owned(),
            url: "https://mcp.example.com/mcp".to_owned(),
            oauth: McpOAuth::default(),
        })
    );
    for name in ["Authorization", "authorization", "AUTHORIZATION"] {
        let static_auth = McpTransportConfig::sse("https://mcp.example.com/sse", headers(&[name]));
        assert_eq!(static_auth.auth_target("docs"), None, "{name}");
    }
    let stdio = McpTransportConfig::stdio("server", Vec::new());
    assert_eq!(stdio.auth_target("local"), None);
}

#[test]
fn a_configured_oauth_replaces_the_default_but_not_an_authorization_header() {
    let configured = McpOAuth {
        client_id: Some("client-1".to_owned()),
        scope: Some("mcp:connect".to_owned()),
        ..McpOAuth::default()
    };
    let http = McpTransportConfig::http("https://mcp.example.com/mcp", BTreeMap::new())
        .with_oauth(Some(configured.clone()));
    assert_eq!(
        http.auth_target("docs").map(|t| t.oauth),
        Some(configured.clone())
    );
    // client_credentials 等不用 OAuth 的写法：None
    let off =
        McpTransportConfig::http("https://mcp.example.com/mcp", BTreeMap::new()).with_oauth(None);
    assert_eq!(off.auth_target("docs"), None);
    let header =
        McpTransportConfig::http("https://mcp.example.com/mcp", headers(&["Authorization"]))
            .with_oauth(Some(configured));
    assert_eq!(header.auth_target("docs"), None);
}

#[test]
fn the_oauth_field_is_optional_on_the_wire_and_keeps_node_field_names() {
    let parsed: McpTransportConfig =
        serde_json::from_value(serde_json::json!({"type": "http", "url": "https://a.example"}))
            .expect("old shape");
    assert_eq!(parsed.auth_target("a"), None);
    let configured =
        McpTransportConfig::http("https://a.example", BTreeMap::new()).with_oauth(Some(McpOAuth {
            client_id: Some("c".to_owned()),
            redirect_path: Some("/cb".to_owned()),
            ..McpOAuth::default()
        }));
    assert_eq!(
        serde_json::to_value(&configured).expect("serialize"),
        serde_json::json!({"type": "http", "url": "https://a.example", "oauth": {"clientId": "c", "redirectPath": "/cb"}})
    );
}
