//! Node 形状的 `mcp.servers`（ext.md §3.1）。
use serde_json::json;

use super::*;

/// 配置文件里服务器表的位置（只出现在警告里）。
const TABLE: &str = "mcp.servers";

#[test]
fn servers_follow_the_node_shape_and_legacy_fields() {
    let (servers, warnings) = parse_node_servers(
        &json!({
            "b-http": {"type": "remote", "url": "https://mcp.example.com/mcp", "http_headers": {"X": "${TOKEN}"}},
            "a-stdio": {"command": "node", "args": ["server.js"], "environment": {"K": "v"}, "timeout": 5},
            "c-sse": {"type": "sse", "url": "https://mcp.example.com/sse"},
            "d-off": {"command": "x", "enable": false, "enabled": true},
            "e-bad": {"type": "stdio"},
            "f-url": {"url": "https://mcp.example.com/u"}
        }),
        McpScope::User,
        TABLE,
    );
    assert_eq!(
        warnings,
        vec!["mcp.servers.e-bad: command: required".to_owned()]
    );
    let names: Vec<_> = servers
        .iter()
        .map(|found| (found.server.name.as_str(), found.enabled))
        .collect();
    assert_eq!(
        names,
        [
            ("a-stdio", true),
            ("b-http", true),
            ("c-sse", true),
            ("d-off", false),
            ("f-url", true)
        ]
    );
    let servers: Vec<_> = servers
        .into_iter()
        .filter(|found| found.enabled)
        .map(|found| found.server)
        .collect();
    assert_eq!(
        servers[0].transport,
        McpTransportConfig::Stdio {
            command: "node".into(),
            args: vec!["server.js".into()],
            env: [("K".to_owned(), "v".to_owned())].into(),
            cwd: None,
        }
    );
    // 值原样使用，不展开 `${VAR}`（Node 不展开）。
    assert_eq!(
        servers[1].transport,
        McpTransportConfig::http(
            "https://mcp.example.com/mcp",
            [("X".to_owned(), "${TOKEN}".to_owned())].into()
        )
    );
    assert!(matches!(
        servers[2].transport,
        McpTransportConfig::Sse { .. }
    ));
    assert!(matches!(
        servers[3].transport,
        McpTransportConfig::Http { .. }
    ));
    assert!(servers.iter().all(|server| server.scope == McpScope::User));
}

#[test]
fn a_non_object_table_is_ignored() {
    let (servers, warnings) = parse_node_servers(&json!([1]), McpScope::Project, TABLE);
    assert!(servers.is_empty());
    assert_eq!(warnings.len(), 1);
}

/// L19：`cwd`、`timeoutMs`、`alwaysLoad` 按 Node 接上；`oauth` 的 `client_credentials` 不做，记警告照常连接（MCP-OAUTH 起
/// `authorization_code` 接上，ext.md §3.12.1，下一个用例）。
/// `protocolVersion` 参与协商、不再记警告（§3.10.2）：`auto`、`legacy` 走 `initialize`，`2026-07-28` 钉死新纪元。
#[test]
fn node_server_fields_are_read_or_reported() {
    let (servers, warnings) = parse_node_servers(
        &json!({
            "a": {"type": "stdio", "command": "node", "cwd": "tools/mcp", "timeoutMs": 45000, "alwaysLoad": true},
            "b": {"type": "http", "url": "https://mcp.example.com/mcp", "timeoutMs": 1.5,
                  "protocolVersion": "auto",
                  "oauth": {"type": "client_credentials", "clientId": "c", "clientSecret": "s"}},
            "c": {"type": "sse", "url": "https://mcp.example.com/sse", "protocolVersion": "legacy"},
            "d": {"type": "stdio", "command": "node", "protocolVersion": "2026-07-28"}
        }),
        McpScope::Project,
        TABLE,
    );
    assert_eq!(
        warnings,
        vec![
            "mcp.servers.b: oauth client_credentials is not supported; connecting without it"
                .to_owned()
        ]
    );
    assert_eq!(
        servers[0].server.transport,
        McpTransportConfig::Stdio {
            command: "node".into(),
            args: Vec::new(),
            env: Default::default(),
            cwd: Some("tools/mcp".into()),
        }
    );
    assert_eq!(
        servers[0].server.options,
        McpServerOptions {
            timeout_ms: Some(45_000),
            always_load: true,
            ..Default::default()
        }
    );
    // 小数毫秒按 Node 的正数接受，向上取整到整毫秒。
    assert_eq!(servers[1].server.options.timeout_ms, Some(2));
    assert_eq!(servers[1].server.options.protocol, McpProtocol::Initialize);
    assert_eq!(servers[2].server.options, McpServerOptions::default());
    assert_eq!(servers[3].server.options.protocol, McpProtocol::Modern);
}

#[test]
fn bad_node_server_fields_make_the_server_invalid() {
    let (servers, warnings) = parse_node_servers(
        &json!({
            "a": {"command": "x", "cwd": ""},
            "b": {"command": "x", "timeoutMs": 0},
            "c": {"command": "x", "alwaysLoad": "yes"},
            "d": {"command": "x", "protocolVersion": "2025-06-18"},
            "e": {"url": "https://mcp.example.com", "cwd": "here"}
        }),
        McpScope::User,
        TABLE,
    );
    assert!(servers.is_empty(), "{servers:?}");
    assert_eq!(
        warnings,
        vec![
            "mcp.servers.a: cwd: expected a non-empty string".to_owned(),
            "mcp.servers.b: timeoutMs: expected a positive number".to_owned(),
            "mcp.servers.c: alwaysLoad: expected a boolean".to_owned(),
            "mcp.servers.d: protocolVersion: expected auto, legacy or 2026-07-28".to_owned(),
            "mcp.servers.e: cwd: only stdio servers have a working directory".to_owned(),
        ]
    );
}

/// `isolation`（Node `McpServerIsolation`，ext.md §3.9）：缺省 `session`；`workspace` 是显式声明的共用；别的值整个服务器
/// 不合法。
#[test]
fn isolation_is_read_from_the_node_field() {
    let (servers, warnings) = parse_node_servers(
        &json!({
            "a-default": {"command": "x"},
            "b-session": {"command": "x", "isolation": "session"},
            "c-shared": {"command": "x", "isolation": "workspace"},
            "d-bad": {"command": "x", "isolation": "process"}
        }),
        McpScope::Project,
        TABLE,
    );
    let isolation: Vec<_> = servers
        .iter()
        .map(|found| (found.server.name.as_str(), found.server.options.isolation))
        .collect();
    assert_eq!(
        isolation,
        [
            ("a-default", McpIsolation::Session),
            ("b-session", McpIsolation::Session),
            ("c-shared", McpIsolation::Workspace),
        ]
    );
    assert_eq!(
        warnings,
        vec!["mcp.servers.d-bad: isolation: expected session or workspace".to_owned()]
    );
}

/// MCP-OAUTH（ext.md §3.12.1）：`authorization_code` 的字段交给传输；没写 `oauth` 时用缺省的；`client_credentials` 与配置的
/// `Authorization` 头不用 OAuth；形状不对（Node 的 strict）时整个服务器不合法。
#[test]
fn oauth_is_read_like_node_s_schema() {
    use rust_cli_kernel_api::mcp::McpOAuth;
    let (servers, warnings) = parse_node_servers(
        &json!({
            "configured": {"type": "http", "url": "https://a.example/mcp",
                "oauth": {"type": "authorization_code", "clientId": "c1", "clientSecret": "s1", "scope": "a b", "redirectPath": "/cb"}},
            "plain": {"type": "sse", "url": "https://b.example/sse"},
            "machine": {"type": "http", "url": "https://c.example/mcp",
                "oauth": {"type": "client_credentials", "clientId": "c", "clientSecret": "s"}},
            "static": {"type": "http", "url": "https://d.example/mcp", "headers": {"Authorization": "Bearer t"}},
            "unknown-key": {"type": "http", "url": "https://e.example/mcp", "oauth": {"type": "authorization_code", "extra": 1}},
            "bad-type": {"type": "http", "url": "https://f.example/mcp", "oauth": {"type": "device_code"}},
            "empty-id": {"type": "http", "url": "https://g.example/mcp", "oauth": {"type": "authorization_code", "clientId": ""}}
        }),
        McpScope::User,
        TABLE,
    );
    let target = |name: &str| {
        servers
            .iter()
            .find(|found| found.server.name == name)
            .and_then(|found| found.server.transport.auth_target(name))
            .map(|target| target.oauth)
    };
    let configured = target("configured").unwrap();
    assert_eq!(configured.client_id.as_deref(), Some("c1"));
    assert_eq!(
        configured
            .client_secret
            .as_ref()
            .map(rust_cli_base::secret::RedactedString::expose),
        Some("s1")
    );
    assert_eq!(configured.scope.as_deref(), Some("a b"));
    assert_eq!(configured.redirect_path.as_deref(), Some("/cb"));
    assert_eq!(target("plain"), Some(McpOAuth::default()));
    assert_eq!(target("machine"), None);
    assert_eq!(target("static"), None);
    let names: Vec<&str> = servers
        .iter()
        .map(|found| found.server.name.as_str())
        .collect();
    assert_eq!(names, ["configured", "machine", "plain", "static"]);
    assert_eq!(
        warnings,
        [
            "mcp.servers.bad-type: oauth.type: expected authorization_code or client_credentials",
            "mcp.servers.empty-id: oauth.clientId: expected a non-empty string",
            "mcp.servers.machine: oauth client_credentials is not supported; connecting without it",
            "mcp.servers.unknown-key: oauth.extra: not allowed",
        ]
    );
}
