//! App 形态按信任记录准入仓库声明的项目级 MCP 服务器（docs/zcode-protocol-v5/18-mcp-trust.md §12；以前是 §6 的止血：一律不连）：
//! 真实进程 `zcode-rs app-server --stdio --protocol v5`，临时 home、临时仓库、本地模型夹具、假凭据。仓库的项目配置声明两个
//! 标记服务器（一个 `isolation: "workspace"`，一个会话级）；用户级配置声明两个，用户级配置引入的插件声明一个。
//! 根因：rust 以前没有 Node 那道逐条信任的门（`workspace-mcp-trust-v1.json`），打开一个仓库就执行了仓库声明的命令。
//!
//! 没有信任记录：订 `ws/<key>/agent`、`session.create` 之后，用户级与插件的服务器照常起，项目级的一个都没起。Node 写下的
//! 记录：两个项目级的都起。`mcp.trust.set`（§12.10）：内核级的补连、新会话的会话级起来，提示随之清掉；改了声明、记录写坏之后
//! 重新组装，都不再起。
//!
//! 第二个测试（18-mcp-trust.md §11）：仓库的项目配置用 `plugins.dirs` 把仓库里的目录当插件引入，这个插件的 MCP 服务器与
//! SessionStart hook 都不起；用户级 `plugins.dirs` 引入的插件照常。
//!
//! 回落 `.agents/mcp.json`（18-mcp-trust.md §14）：仓库只有这个文件时，它的服务器是项目级声明、照样经信任门：没有记录一个都不起；
//! `mcp.trust.set` 认 Node 算的 digest（以前回 `mcpDeclarationChanged`）；Node 格式的记录让好的起；不合法的邻项从来不起，也不拖垮好的。
//!
//! 用户级的回落 `~/.agents/mcp.json`（18-mcp-trust.md §14.10）：用户级配置没有 MCP 条目时读它，它的服务器是用户级、不经信任门就起；
//! 与仓库里的声明同名时排在项目级之下，仓库的那个照样待信任。
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

use project_mcp::*;
use serde_json::{Value, json};

#[test]
fn the_app_does_not_start_untrusted_project_mcp_servers() {
    let place = Place::new();
    place.configure();
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    let repo = place.repo.to_string_lossy().into_owned();
    // ① 订工作区的 agent topic：组装这个仓库的内核，开始连内核级（`isolation: "workspace"`）的服务器。
    host.open_workspace(&repo);
    host.wait_until("the user-level shared server", |_| {
        place.starts_of(USER_SHARED) == 1
    });
    // ② 建会话（不带输入）：会话 actor 启动，开始连每会话的服务器（插件的服务器也是每会话的）。
    host.create_session(&repo);
    host.wait_until("the user-level and plugin session servers", |_| {
        place.starts_of(USER_SESSION) == 1 && place.starts_of(PLUGIN) == 1
    });
    host.close();
    // 项目级的两个与用户级、插件的服务器同批开始连接（①、② 各一批）；进程退出之后，它们一个都没起过。
    let started = place.started();
    assert_eq!(place.starts_of(PROJECT_SHARED), 0, "{started:?}");
    assert_eq!(place.starts_of(PROJECT_SESSION), 0, "{started:?}");
    assert_eq!(place.starts_of(USER_SHARED), 1, "{started:?}");
    assert_eq!(place.starts_of(USER_SESSION), 1, "{started:?}");
    assert_eq!(place.starts_of(PLUGIN), 1, "{started:?}");
}

#[test]
fn the_app_does_not_load_plugins_from_project_plugin_dirs() {
    let place = Place::new();
    place.configure_plugin_dirs();
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    let repo = place.repo.to_string_lossy().into_owned();
    host.open_workspace(&repo);
    host.create_session(&repo);
    host.wait_until("the user plugin's server and SessionStart hook", |_| {
        place.starts_of(PLUGIN) == 1 && place.hooks_of(USER_HOOK) == 1
    });
    host.close();
    // 两个插件的服务器在同一批里开始连接，两个插件的 SessionStart hook 在同一批里跑（hooks 引擎并行起一批、等它们都结束）；
    // 进程退出之后，仓库里的插件一样都没起过。
    let log = std::fs::read_to_string(&place.mcp_log).unwrap_or_default();
    assert_eq!(place.starts_of(REPO_PLUGIN), 0, "{log}");
    assert_eq!(place.hooks_of(REPO_HOOK), 0, "{log}");
    assert_eq!(place.starts_of(PLUGIN), 1, "{log}");
    assert_eq!(place.hooks_of(USER_HOOK), 1, "{log}");
}

/// Node 写下的记录（`JSON.stringify` 的紧凑写法，18 §12.3）rust 认：两份声明都有记录，内核级的在组装时起，会话级的在建会话时起。
#[test]
fn a_record_written_in_the_node_format_admits_the_declarations() {
    let place = Place::new();
    place.configure_trust(false);
    let repo = place.repo.to_string_lossy().into_owned();
    let record = |name: &str| {
        json!({
            "workspaceIdentity": repo,
            "serverName": name,
            "declarationDigest": place.digest(name),
            "grantedAt": "2026-11-05T08:00:00.000Z",
        })
    };
    write(
        &place.trust_store(),
        &json!({"schemaVersion": 1, "records": [record(PROJECT_SHARED), record(PROJECT_SESSION)]}),
    );
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    host.open_workspace(&repo);
    host.wait_until("the trusted workspace-level server", |_| {
        place.starts_of(PROJECT_SHARED) == 1 && place.starts_of(USER_SHARED) == 1
    });
    host.create_session(&repo);
    host.wait_until("the trusted session server", |_| {
        place.starts_of(PROJECT_SESSION) == 1 && place.starts_of(USER_SESSION) == 1
    });
    host.close();
}

/// 18 §12.10 的四种情形，一个进程里依次走：没信任 → `mcp.trust.set` → 改了声明 → 记录写坏。不出现的判定没有计时器：项目级的与用户级的
/// 服务器在同一批里开始连接（订阅、`agent.release` 之后的重新组装是内核级的一批，建会话是会话级的一批），等用户级的标记出现，最后关掉进程、
/// 等它退出之后数标记。
#[test]
fn trusting_a_declaration_starts_it_until_the_declaration_or_the_record_changes() {
    let place = Place::new();
    place.configure_trust(false);
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    let repo = place.repo.to_string_lossy().into_owned();
    // ① 没信任：用户级的起来；会话的提示是两个待信任的声明
    host.open_workspace(&repo);
    host.create_session_as(&repo, "ses_one", "019a5f3c-7a2b-7c3d-8e4f-0000000000b1");
    host.watch_session(&repo, "ses_one", "one");
    host.wait_until("the user-level servers of the first session", |_| {
        place.starts_of(USER_SHARED) == 1 && place.starts_of(USER_SESSION) == 1
    });
    let pending = host
        .mcp_admission("one")
        .expect("the session snapshot has mcpAdmission");
    assert_eq!(pending["pendingCount"], 2, "{pending}");
    // ② 信任：内核级的补连一次；ACK 之前活会话的提示已经换了（效果先于 ACK）
    let shared = host.trust(&repo, PROJECT_SHARED, &place.digest(PROJECT_SHARED), true);
    assert_eq!(
        (shared["kind"].clone(), shared["noop"].clone()),
        (json!("accepted"), json!(false)),
        "{shared}"
    );
    assert_eq!(host.mcp_admission("one").unwrap()["pendingCount"], 1);
    assert!(
        shared["effects"].to_string().contains("ses_one"),
        "{shared}"
    );
    host.wait_until("the trusted workspace-level server", |_| {
        place.starts_of(PROJECT_SHARED) == 1
    });
    let session = host.trust(&repo, PROJECT_SESSION, &place.digest(PROJECT_SESSION), true);
    assert_eq!(session["kind"], "accepted", "{session}");
    assert_eq!(host.mcp_admission("one"), Some(Value::Null));
    // 重放：设值语义
    let again = host.trust(&repo, PROJECT_SESSION, &place.digest(PROJECT_SESSION), true);
    assert_eq!(again["noop"], true, "{again}");
    // 会话级的不在已打开的会话里重放；下一个会话打开时连
    host.create_session_as(&repo, "ses_two", "019a5f3c-7a2b-7c3d-8e4f-0000000000b2");
    host.wait_until("the trusted session server in the next session", |_| {
        place.starts_of(PROJECT_SESSION) == 1 && place.starts_of(USER_SESSION) == 2
    });
    // ③ 改了声明（第二版没有信任记录）、重新组装：没改的内核级照样起，改了的那个不起
    place.configure_trust(true);
    host.reassemble(&repo, "agent-v2");
    host.wait_until("the kernel-level servers after reassembly", |_| {
        place.starts_of(USER_SHARED) == 2 && place.starts_of(PROJECT_SHARED) == 2
    });
    host.create_session_as(&repo, "ses_three", "019a5f3c-7a2b-7c3d-8e4f-0000000000b3");
    host.wait_until("the user session server of the third session", |_| {
        place.starts_of(USER_SESSION) == 3
    });
    // ④ 记录写坏：全部未信任；mcp.trust.set 拒绝且不覆盖
    let corrupt = "{\"schemaVersion\":1,\"records\":[{}]}";
    std::fs::write(place.trust_store(), corrupt).unwrap();
    host.reassemble(&repo, "agent-corrupt");
    host.create_session_as(&repo, "ses_four", "019a5f3c-7a2b-7c3d-8e4f-0000000000b4");
    host.wait_until("the user-level servers after the record broke", |_| {
        place.starts_of(USER_SHARED) == 3 && place.starts_of(USER_SESSION) == 4
    });
    let refused = host.trust(&repo, PROJECT_SESSION, &place.digest(PROJECT_SESSION), true);
    assert_eq!(
        refused["code"], "precondition.mcpTrustUnavailable",
        "{refused}"
    );
    assert_eq!(
        std::fs::read_to_string(place.trust_store()).unwrap(),
        corrupt
    );
    host.close();
    let started = place.started();
    // 内核级：补连一次、重新组装一次；记录坏了之后不起
    assert_eq!(place.starts_of(PROJECT_SHARED), 2, "{started:?}");
    // 会话级：只在信任之后的第二个会话里起过；改过的声明一次都没起
    assert_eq!(place.starts_of(PROJECT_SESSION), 1, "{started:?}");
    assert_eq!(place.starts_of(PROJECT_SESSION_V2), 0, "{started:?}");
}

/// 只有回落文件的仓库，没有记录：一个都不起，提示数两个合法的声明（不合法的邻项不是声明）；`mcp.trust.set` 收下 Node 算的 digest，
/// 内核级的那个补连起来。以前 rust 不读这个文件：提示没有它们，信任回 `precondition.mcpDeclarationChanged`。
#[test]
fn the_agents_file_is_a_project_declaration_behind_the_trust_gate() {
    let place = Place::new();
    place.configure_agents();
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    let repo = place.repo.to_string_lossy().into_owned();
    host.open_workspace(&repo);
    host.create_session(&repo);
    host.watch_session(&repo, SESSION, "session");
    // 回落的服务器与用户级的同批开始连接（内核级在订阅时、每会话的在建会话时）
    host.wait_until("the user-level servers", |_| {
        place.starts_of(USER_SHARED) == 1 && place.starts_of(USER_SESSION) == 1
    });
    let pending = host
        .mcp_admission("session")
        .expect("the session snapshot has mcpAdmission");
    assert_eq!(pending["pendingCount"], 2, "{pending}");
    let trusted = host.trust(
        &repo,
        AGENTS_SHARED,
        &place.agents_digest(AGENTS_SHARED),
        true,
    );
    assert_eq!(
        (trusted["kind"].clone(), trusted["noop"].clone()),
        (json!("accepted"), json!(false)),
        "{trusted}"
    );
    assert_eq!(host.mcp_admission("session").unwrap()["pendingCount"], 1);
    host.wait_until("the trusted workspace-level server", |_| {
        place.starts_of(AGENTS_SHARED) == 1
    });
    host.close();
    let started = place.started();
    assert_eq!(place.starts_of(AGENTS_SESSION), 0, "{started:?}");
    assert_eq!(place.starts_of(AGENTS_BROKEN), 0, "{started:?}");
}

/// Node 格式写下的回落声明的记录：两个合法的都起。不合法的邻项也照同一写法记了一条（rust 若把它当成服务器，它就有记录、会起），
/// 它与内核级的那个同批，从来不起。
#[test]
fn a_node_record_admits_the_agents_file_and_a_broken_neighbour_stays_out() {
    let place = Place::new();
    place.configure_agents();
    let repo = place.repo.to_string_lossy().into_owned();
    let record = |name: &str| {
        json!({
            "workspaceIdentity": repo,
            "serverName": name,
            "declarationDigest": place.agents_digest(name),
            "grantedAt": "2026-11-06T08:00:00.000Z",
        })
    };
    write(
        &place.trust_store(),
        &json!({"schemaVersion": 1, "records": [
            record(AGENTS_SHARED),
            record(AGENTS_SESSION),
            record(AGENTS_BROKEN),
        ]}),
    );
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    host.open_workspace(&repo);
    host.wait_until("the trusted workspace-level server", |_| {
        place.starts_of(AGENTS_SHARED) == 1 && place.starts_of(USER_SHARED) == 1
    });
    host.create_session(&repo);
    host.wait_until("the trusted session server", |_| {
        place.starts_of(AGENTS_SESSION) == 1 && place.starts_of(USER_SESSION) == 1
    });
    host.close();
    let started = place.started();
    assert_eq!(place.starts_of(AGENTS_BROKEN), 0, "{started:?}");
}

/// 只有 `~/.agents/mcp.json` 的用户：只在那里的服务器不用信任就起（用户级）。与仓库里 `project-shared` 同名的那一个不用（项目级优先，
/// v4 App 去重时工作区优先），仓库的 `project-shared` 没有记录、也不起，提示里照样有它。以前 rust 不读这个文件：一个都不起。
#[test]
fn the_user_agents_file_is_user_level_and_below_the_project_level() {
    let place = Place::new();
    place.configure_user_agents();
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    let repo = place.repo.to_string_lossy().into_owned();
    host.open_workspace(&repo);
    // 三个都是内核级，在组装工作区时同一批开始连
    host.wait_until("the user-level server from ~/.agents/mcp.json", |_| {
        place.starts_of(USER_AGENTS) == 1
    });
    host.create_session(&repo);
    host.watch_session(&repo, SESSION, "session");
    host.wait_until("the session snapshot", |host| {
        host.mcp_admission("session").is_some()
    });
    let pending = host.mcp_admission("session").unwrap();
    assert_eq!(pending["pendingCount"], 1, "{pending}");
    host.close();
    let started = place.started();
    assert_eq!(place.starts_of(USER_AGENTS), 1, "{started:?}");
    assert_eq!(place.starts_of(USER_AGENTS_COPY), 0, "{started:?}");
    assert_eq!(place.starts_of(PROJECT_SHARED), 0, "{started:?}");
}
