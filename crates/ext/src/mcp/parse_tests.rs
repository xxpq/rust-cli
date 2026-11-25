//! MCP 配置的解析、`${VAR}` 展开与作用域合并（ext.md §3.1）。

use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::json;

use super::*;

fn lookup(name: &str) -> Option<String> {
    (name == "TOKEN").then(|| "secret".to_owned())
}

#[test]
fn servers_parse_with_variables_expanded() {
    let table = json!({
        "web": {"type": "http", "url": "https://mcp.example.com/${PATH_PART:-mcp}", "headers": {"authorization": "Bearer ${TOKEN}"}},
        "files": {"command": "node", "args": ["server.js", "${MISSING}"], "env": {"KEY": "${TOKEN}"}},
        "bad": {"type": "ws", "url": "wss://x"},
        "nocmd": {"type": "stdio"}
    });
    let (servers, warnings) = parse_servers(&table, McpScope::Project, &lookup);
    let names: Vec<&str> = servers.iter().map(|server| server.name.as_str()).collect();
    assert_eq!(names, ["files", "web"]);
    let McpTransportConfig::Stdio { args, env, .. } = &servers[0].transport else {
        panic!("files is stdio");
    };
    assert_eq!(args, &["server.js", "${MISSING}"]);
    assert_eq!(env["KEY"], "secret");
    let McpTransportConfig::Http { url, headers, .. } = &servers[1].transport else {
        panic!("web is http");
    };
    assert_eq!(url, "https://mcp.example.com/mcp");
    assert_eq!(headers["authorization"], "Bearer secret");
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings.iter().any(|warning| warning.contains("MISSING")));
}

#[test]
fn servers_bound_to_a_remote_machine_keep_their_variables() {
    // R5（remote-exec.md §22.7）：绑定到远端机器的服务器按原文解析，本机的值（这里的 TOKEN）不进请求。
    let table = json!({
        "remote": {"command": "server", "args": ["${TOKEN}"], "env": {"KEY": "${TOKEN:-none}"}},
        "here": {"command": "server", "args": ["${TOKEN}"]}
    });
    let (servers, warnings) =
        parse_servers_keeping(&table, McpScope::Dynamic, &lookup, &|name| name == "remote");
    assert!(warnings.is_empty(), "{warnings:?}");
    let McpTransportConfig::Stdio { args, .. } = &servers[0].transport else {
        panic!("here is stdio");
    };
    assert_eq!(args, &["secret"]);
    let McpTransportConfig::Stdio { args, env, .. } = &servers[1].transport else {
        panic!("remote is stdio");
    };
    assert_eq!(args, &["${TOKEN}"]);
    assert_eq!(env["KEY"], "${TOKEN:-none}");
}

fn server(name: &str, scope: McpScope, command: &str) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope,
        transport: McpTransportConfig::stdio(command.to_owned(), Vec::new()),
        options: Default::default(),
        machine: Default::default(),
    }
}

#[test]
fn local_beats_project_beats_user_and_enterprise_is_exclusive() {
    let merged = merge_scopes(vec![
        vec![
            server("a", McpScope::User, "user"),
            server("b", McpScope::User, "user"),
        ],
        vec![server("a", McpScope::Project, "project")],
        vec![
            server("a", McpScope::Local, "local"),
            server("c", McpScope::Local, "local"),
        ],
    ]);
    let summary: Vec<(String, McpScope)> = merged
        .iter()
        .map(|server| (server.name.clone(), server.scope))
        .collect();
    assert_eq!(
        summary,
        [
            ("a".to_owned(), McpScope::Local),
            ("b".to_owned(), McpScope::User),
            ("c".to_owned(), McpScope::Local)
        ]
    );
    let only = merge_scopes(vec![
        vec![server("x", McpScope::Enterprise, "corp")],
        vec![server("y", McpScope::User, "user")],
    ]);
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].name, "x");
}

/// MCP-OAUTH（ext.md §3.12.1）：`mcpServers` 写法的 http / sse 服务器缺省用 OAuth，认 `oauth.clientId`；配置了 Authorization 头时不用。
#[test]
fn mcp_servers_entries_use_oauth_and_read_the_client_id() {
    let table = json!({
        "pre": {"type": "http", "url": "https://a.example/mcp", "oauth": {"clientId": "cc-1", "callbackPort": 9000}},
        "plain": {"type": "sse", "url": "https://b.example/sse"},
        "static": {"type": "http", "url": "https://c.example/mcp", "headers": {"Authorization": "Bearer t"}}
    });
    let (servers, _) = parse_servers(&table, McpScope::Dynamic, &lookup);
    let oauth = |name: &str| {
        let server = servers.iter().find(|server| server.name == name).unwrap();
        server
            .transport
            .auth_target(name)
            .map(|target| target.oauth)
    };
    assert_eq!(
        oauth("pre").and_then(|o| o.client_id).as_deref(),
        Some("cc-1")
    );
    assert_eq!(
        oauth("plain"),
        Some(rust_cli_kernel_api::mcp::McpOAuth::default())
    );
    assert_eq!(oauth("static"), None);
}

/// MCP-OAUTH-2（ext.md §3.12.15）：`callbackPort`、`authServerMetadataUrl`、`scopes` 进 OAuth 配置；形状不对时整个服务器不合法。
#[test]
fn oauth_callback_port_metadata_url_and_scopes_are_read_and_checked() {
    let table = json!({
        "full": {"type": "http", "url": "https://a.example/mcp", "oauth": {
            "clientId": "cc-1", "callbackPort": 9000,
            "authServerMetadataUrl": "https://auth.example.com/.well-known/oauth-authorization-server",
            "scopes": "read write"}},
        "plain_http_metadata": {"type": "http", "url": "https://b.example/mcp",
            "oauth": {"authServerMetadataUrl": "http://auth.example.com/meta"}},
        "zero_port": {"type": "http", "url": "https://c.example/mcp", "oauth": {"callbackPort": 0}},
        "huge_port": {"type": "http", "url": "https://d.example/mcp", "oauth": {"callbackPort": 70000}},
        "empty_scopes": {"type": "http", "url": "https://e.example/mcp", "oauth": {"scopes": ""}}
    });
    let (servers, warnings) = parse_servers(&table, McpScope::Dynamic, &lookup);
    let names: Vec<&str> = servers.iter().map(|server| server.name.as_str()).collect();
    assert_eq!(names, ["full"]);
    let oauth = servers[0].transport.auth_target("full").unwrap().oauth;
    assert_eq!(oauth.client_id.as_deref(), Some("cc-1"));
    assert_eq!(oauth.callback_port, Some(9000));
    assert_eq!(
        oauth.metadata_url.as_deref(),
        Some("https://auth.example.com/.well-known/oauth-authorization-server")
    );
    assert_eq!(oauth.scope.as_deref(), Some("read write"));
    assert_eq!(warnings.len(), 4, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("authServerMetadataUrl must use https://")),
        "{warnings:?}"
    );
}
