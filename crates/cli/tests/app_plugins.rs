//! v5 的插件管理接上真实实现之后（docs/zcode-protocol-v5/24-plugins.md §13；docs/specs/rust-cli/protocol-v5.md §72.7–§72.9）：
//! 真实进程 `zcode-rs app-server --stdio --protocol v5`，临时 home、本地目录市场、本地模型夹具、假凭据。市场里的插件 `kit` 带一个
//! 每会话的标记服务器与一个 SessionStart hook（`tests/fixtures/mcp-counter.mjs` 的标记日志）；用户级配置另声明一个每会话的标记服务器，
//! 每个会话起一次，当作「这个会话已经起来了」的屏障。
//!
//! 握手声明 `catalog.plugins`；会话 A 开着时装上 kit：之后建的 B 起了 kit 的服务器、跑了 hook，A 没有；用户层停用之后建的 C 没有，
//! B 的那个服务器照旧开着（开着的会话不换插件）。冷读过、还没打开的会话在装上之后再打开，用的是新的插件（通道只在会话开着时钉内核）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、找 node、起真实进程与本地夹具、检查写出的文件"
)]

mod app_host;
mod sse;
mod system_env;

mod project_mcp;

use std::path::PathBuf;

use project_mcp::*;
use rust_cli_protocol_v5::generated::contract::CONTRACT_DIGEST;
use serde_json::{Value, json};

const KIT: &str = "kit@mkt";
const SESSION_A: &str = "ses_plugins_a";
const SESSION_B: &str = "ses_plugins_b";
const SESSION_C: &str = "ses_plugins_c";
/// 会话 A 的 topic 订阅名。
const SESSION_SUB: &str = "a";
/// 用户级插件 topic 的订阅名。
const PLUGINS_SUB: &str = "u";
/// kit 的敏感配置项与写给它的值（不该出现在帧里）。
const TOKEN: &str = "token";
const SECRET: &str = "s3cret-app-plugins";

/// 本地目录市场 `mkt`（插件 `kit`）与用户级配置的屏障服务器；返回市场的目录。
fn market(place: &Place) -> PathBuf {
    let market = place.home.parent().unwrap().join("market");
    write(
        &market.join(".claude-plugin/marketplace.json"),
        &json!({"name": "mkt", "owner": {"name": "Dev"}, "plugins": [{"name": "kit", "source": "./kit"}]}),
    );
    place.write_plugin(&market.join("kit"), "kit", PLUGIN, USER_HOOK);
    write(
        &market.join("kit/.zcode-plugin/plugin.json"),
        &json!({"name": "kit", "userConfig": {TOKEN: {"type": "string", "sensitive": true}}}),
    );
    write(
        &place.home.join(".zcode/cli/config.json"),
        &json!({"mcp": {"servers": {USER_SESSION: place.server(USER_SESSION, None)}}}),
    );
    market
}

/// 握手（严格链路）并要插件管理：rust 声明了它。
fn initialize(host: &mut V5Host) {
    let result = host.request(
        "initialize",
        json!({
            "clientId": "test-host",
            "peer": {"kind": "host", "name": "test-host", "build": {"version": "test"}},
            "caps": {"core": [1], "agent": [1], "session": [1], "catalog.plugins": [1]},
            "require": ["core", "agent", "session"],
            "contract": CONTRACT_DIGEST,
        }),
    );
    assert_eq!(result["caps"]["catalog.plugins"], 1, "{result}");
}

/// 一条插件命令（Host 级，用户层）：等 ACK，要 accepted。
fn plugins(host: &mut V5Host, command: &str, kind: &str, payload: Value) {
    let ack = host.request(
        "command",
        json!({"commandId": command, "type": kind, "target": {"hostId": "local"}, "payload": payload}),
    );
    assert_eq!(ack["kind"], "accepted", "{kind}: {ack}");
}

/// 加市场、装 kit。
fn install(host: &mut V5Host, market: &std::path::Path) {
    plugins(
        host,
        "plugins-add",
        "plugins.marketplace.add",
        json!({"source": market}),
    );
    plugins(
        host,
        "plugins-install",
        "plugins.install",
        json!({"pluginId": KIT}),
    );
}

/// 标记日志里这个服务器退出过几次。
fn exits_of(place: &Place, name: &str) -> usize {
    std::fs::read_to_string(&place.mcp_log)
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            line.strip_prefix("exit ")
                .and_then(|rest| rest.split(' ').next())
                == Some(name)
        })
        .count()
}

#[test]
fn new_sessions_load_the_changed_plugins_and_open_sessions_keep_theirs() {
    let place = Place::new();
    let market = market(&place);
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    initialize(&mut host);
    let repo = place.repo.to_string_lossy().into_owned();
    host.open_workspace(&repo);
    host.create_session_as(&repo, SESSION_A, "019a5f3c-7a2b-7c3d-8e4f-00000000b0a1");
    host.wait_until("session A", |_| place.starts_of(USER_SESSION) == 1);
    host.request(
        "subscribe",
        json!({"sub": PLUGINS_SUB, "topic": "catalog/plugins"}),
    );
    install(&mut host, &market);
    // 敏感值只进私有文件：topic 里只有键名
    plugins(
        &mut host,
        "plugins-token",
        "plugins.configure",
        json!({"pluginId": KIT, "scope": "user", "set": {TOKEN: SECRET}}),
    );
    assert!(!host.frames_contain(SECRET));
    let secrets =
        std::fs::read_to_string(place.home.join(".zcode/cli/plugin-secrets.json")).unwrap();
    assert!(secrets.contains(SECRET), "{secrets}");
    // 装上之后建的会话：起 kit 的服务器、跑它的 hook
    host.create_session_as(&repo, SESSION_B, "019a5f3c-7a2b-7c3d-8e4f-00000000b0a2");
    host.wait_until("session B with the plugin", |_| {
        place.starts_of(USER_SESSION) == 2
            && place.starts_of(PLUGIN) == 1
            && place.hooks_of(USER_HOOK) == 1
    });
    // 用户层停用：之后建的会话没有它；B 开着，它的服务器不停
    plugins(
        &mut host,
        "plugins-off",
        "plugins.setEnabled",
        json!({"pluginId": KIT, "scope": "user", "enabled": false}),
    );
    host.create_session_as(&repo, SESSION_C, "019a5f3c-7a2b-7c3d-8e4f-00000000b0a3");
    host.wait_until("session C", |_| place.starts_of(USER_SESSION) == 3);
    let exits_before_close = exits_of(&place, PLUGIN);
    host.close();
    let started = place.started();
    // 只有 B 起过 kit 的服务器（A 在装之前开着，C 在停用之后建）
    assert_eq!(place.starts_of(PLUGIN), 1, "{started:?}");
    assert_eq!(place.hooks_of(USER_HOOK), 1, "{started:?}");
    assert_eq!(exits_before_close, 0, "{started:?}");
}

#[test]
fn a_session_read_cold_before_the_install_opens_with_the_new_plugins() {
    let place = Place::new();
    let market = market(&place);
    let repo = place.repo.to_string_lossy().into_owned();
    // 第一个进程：建会话 A（当场落库），退出
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    initialize(&mut host);
    host.open_workspace(&repo);
    let ack = host.request(
        "command",
        json!({
            "commandId": "019a5f3c-7a2b-7c3d-8e4f-00000000c0a1",
            "type": "session.create",
            "target": {"hostId": "local", "workspaceKey": repo},
            "payload": {"sessionId": SESSION_A, "persistence": "immediate"},
        }),
    );
    assert_eq!(ack["kind"], "accepted", "{ack}");
    host.wait_until("session A", |_| place.starts_of(USER_SESSION) == 1);
    host.close();
    // 第二个进程：先冷读 A（订它的 topic，不打开），再装 kit，然后发一条输入打开 A
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    let mut host = V5Host::start(&place, &fixture.url);
    initialize(&mut host);
    host.open_workspace(&repo);
    host.watch_session(&repo, SESSION_A, SESSION_SUB);
    install(&mut host, &market);
    let ack = host.request(
        "command",
        json!({
            "commandId": "019a5f3c-7a2b-7c3d-8e4f-00000000c0a2",
            "type": "input.send",
            "target": {"hostId": "local", "workspaceKey": repo, "sessionId": SESSION_A},
            "payload": {"text": "hi"},
        }),
    );
    assert_eq!(ack["kind"], "accepted", "{ack}");
    // 以前冷读时通道就钉住了那时的内核，A 打开时没有 kit
    host.wait_until("session A reopened with the plugin", |_| {
        place.starts_of(PLUGIN) == 1 && place.hooks_of(USER_HOOK) == 1
    });
    host.close();
}
