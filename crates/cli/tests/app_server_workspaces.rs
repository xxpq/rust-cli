//! App 里一个 rust 进程服务所有本地工作区（工作线 F145，docs/specs/rust-cli/shared-workspaces.md）：真实进程
//! `zcode-rs app-server --stdio`，cwd 是数据目录（Host 给共享进程的固定目录），两个工作区各有项目配置。每个工作区的
//! hooks、deny 规则、缺省模式（后两者来自本机项目设置；项目文件的 `permission` 不读，config.md §1.1）与记忆目录只作用于自己的会话；每个会话一套 MCP 连接，声明 `isolation: "workspace"` 的
//! 服务器同一工作区只起一份，会话关闭后它的连接结束；项目级的 MCP 声明不连（止血，docs/zcode-protocol-v5/18-mcp-trust.md
//! §6），所以 MCP 服务器来自用户级配置；会话请求按 Host 登记的工作区路由，冲突与没有登记的拒绝；数据目录的项目配置里的
//! `storage.sessionDbPath` 不生效。临时 home、本地模型夹具、假凭据。
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

use std::path::{Path, PathBuf};

use app_host::{Env, Host, envelope, eventually, workspace_ref};
use rust_cli_base::models::GLM_5_3;
use serde_json::{Value, json};

const CONNECTION: &str = "test-connection";

/// 测试进程的 PATH 里的 node（`pnpm test:rust-cli` 由 node 启动，PATH 里一定有）。
fn node() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
        .expect("node is not on PATH")
}

fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value.to_string()).unwrap();
}

/// 临时目录：home、数据目录（进程 cwd）、两个工作区与日志。
struct Place {
    _dir: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
    data: PathBuf,
    a: PathBuf,
    b: PathBuf,
    /// MCP 替身的起停记录。
    mcp_log: PathBuf,
    /// 两个工作区的 hook 各写一行自己的字母。
    hook_log: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let place = Self {
            home: base.join("home"),
            data: base.join("data"),
            a: base.join("ws-a"),
            b: base.join("ws-b"),
            mcp_log: base.join("mcp.log"),
            hook_log: base.join("hooks.log"),
            base,
            _dir: dir,
        };
        for dir in [&place.home, &place.data] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for ws in [&place.a, &place.b] {
            std::fs::create_dir_all(ws.join(".git")).unwrap();
        }
        place
    }

    fn env<'a>(&'a self, model_url: &'a str) -> Env<'a> {
        Env {
            home: &self.home,
            cwd: &self.data,
            model_url,
            model: GLM_5_3,
        }
    }

    /// 一个 MCP 替身服务器的配置（`isolation` 为 None 时不写这个字段）。
    fn mcp_server(&self, name: &str, isolation: Option<&str>) -> Value {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-counter.mjs");
        let mut server =
            json!({"type": "stdio", "command": node(), "args": [script, self.mcp_log, name]});
        if let Some(isolation) = isolation {
            server["isolation"] = json!(isolation);
        }
        server
    }

    /// UserPromptSubmit 时往 hook 日志追加一行 `letter`。
    fn hook(&self, letter: &str) -> Value {
        let line = format!("echo {letter} >> '{}'", self.hook_log.display());
        json!({"enabled": true, "events": {"UserPromptSubmit": [{"hooks": [
            {"type": "process", "command": "/bin/sh", "args": ["-c", line]}]}]}})
    }

    /// 两个工作区的配置：A 的本机项目设置（db 的 `local_setting`，按工作区）是缺省 edit 模式、整个 Bash 被拒；B 的项目文件写了
    /// 同样想放权的 `permission`（yolo、拒绝 Bash），项目文件只读扩展声明，它不生效（config.md §1.1，PROJECT-CONFIG；以前 A 的
    /// 模式与规则写在项目文件里，App 按工作区读它）。两边的项目 hooks 经 `zcode hooks trust grant` 授权；各声明一个项目级 MCP
    /// 服务器（A 的 `project-a` 是会话级，B 的 `project-b` 声明 `isolation: "workspace"`），App 形态不连它们（18-mcp-trust.md §6）。
    /// 用户级配置的 MCP：alpha（会话级）、beta（声明 `isolation: "workspace"`）。
    fn configure(&self) {
        write(
            &self.a.join(".zcode/config.json"),
            &json!({
                "hooks": self.hook("a"),
                "mcp": {"servers": {"project-a": self.mcp_server("project-a", None)}},
            }),
        );
        self.seed_project_settings(&self.a);
        write(
            &self.b.join(".zcode/config.json"),
            &json!({
                "permission": {"mode": "yolo", "disallowedTools": ["Bash"]},
                "hooks": self.hook("b"),
                "mcp": {"servers": {"project-b": self.mcp_server("project-b", Some("workspace"))}},
            }),
        );
        write(
            &self.home.join(".zcode/cli/config.json"),
            &json!({"mcp": {"servers": {
                "alpha": self.mcp_server("alpha", None),
                "beta": self.mcp_server("beta", Some("workspace")),
            }}}),
        );
        for ws in [&self.a, &self.b] {
            self.grant_hooks(ws);
        }
    }

    fn zcode(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        use system_env::KeepSystem as _;
        rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .output()
            .unwrap()
    }

    /// 这个工作区的本机项目设置：持久模式 acceptEdits（Node 的 edit）、规则集拒绝整个 Bash。与用户在会话里切模式、选“总是拒绝”
    /// 写的是同一处（config.md §2.2、shell-permissions.md §5），经一个种子会话的事实提交。
    fn seed_project_settings(&self, ws: &Path) {
        use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule, RuleBehavior};
        use rust_cli_kernel_api::store::{Fact, FactBatch, SessionKind, SessionMeta, SessionStore};
        let paths =
            rust_cli_paths::StoragePaths::resolve(&Default::default(), &self.home, &self.data);
        let store = rust_cli_node_store::NodeStore::new(
            paths,
            std::sync::Arc::new(rust_cli_node_store::NodeIds::new(std::sync::Arc::new(
                rust_cli_base::clock::SystemClock::utc(),
            ))),
        );
        let id = rust_cli_kernel_api::ids::SessionId::new("sess_seed_settings");
        let meta = SessionMeta {
            id: id.clone(),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: rust_cli_kernel_api::env::WorkspaceRef {
                environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
                root: rust_cli_kernel_api::env::EnvPath::new(ws.to_string_lossy()),
            },
            title: String::new(),
            trace: rust_cli_kernel_api::ids::TraceId::new("sess_seed_settings"),
            created_at_ms: 0,
        };
        let batch = FactBatch {
            command: None,
            facts: vec![
                Fact::ProjectModeSet {
                    mode: PermissionMode::AcceptEdits,
                },
                Fact::PermissionRulesAdded {
                    behavior: RuleBehavior::Deny,
                    rules: vec![PermissionRule {
                        tool: "Bash".to_owned(),
                        content: None,
                    }],
                },
            ],
            turn: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            store
                .create(meta, rust_cli_kernel_api::testing::no_facts())
                .await
                .unwrap();
            store.commit(&id, batch).await.unwrap();
        });
    }

    /// 授权这个工作区当前的项目 hooks（与用户在 App 或 Node CLI 里授权写的是同一份记录）。
    fn grant_hooks(&self, ws: &Path) {
        let review = self.zcode(ws, &["hooks", "trust", "review", "--json"]);
        let status: Value = serde_json::from_slice(&review.stdout).unwrap();
        let bundle = status["bundleDigest"].as_str().unwrap().to_owned();
        let grant = self.zcode(
            ws,
            &[
                "hooks",
                "trust",
                "grant",
                "--all-current",
                "--bundle-digest",
                &bundle,
            ],
        );
        assert!(
            grant.status.success(),
            "{}",
            String::from_utf8_lossy(&grant.stderr)
        );
    }

    fn mcp_lines(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.mcp_log)
            .unwrap_or_default()
            .lines()
            .map(|line| line.split(' ').map(str::to_owned).collect())
            .collect()
    }

    /// 起过的 MCP 进程：（服务器名，pid，cwd）。
    fn mcp_starts(&self) -> Vec<(String, String, String)> {
        self.mcp_lines()
            .into_iter()
            .filter(|line| line.first().is_some_and(|word| word == "start"))
            .map(|line| (line[1].clone(), line[2].clone(), line[3..].join(" ")))
            .collect()
    }

    fn hook_letters(&self) -> Vec<String> {
        std::fs::read_to_string(&self.hook_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn memory_dir(&self, ws: &Path) -> String {
        rust_cli_paths::StoragePaths::resolve(&Default::default(), &self.home, &self.data)
            .memory_dir(&ws.to_string_lossy())
            .as_path()
            .display()
            .to_string()
    }
}

/// 进程还在不在（`kill -0`）。
fn alive(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// 走 Host 的握手：两次 `runtime/capabilities`（第二次声明 Host 支持按工作区路由）。返回第一次的结果。
fn negotiate(host: &mut Host) -> Value {
    let first = host.request(
        "runtime/capabilities",
        json!({"hostCapabilities": {"sessionDirectorySource": true}}),
    );
    host.request(
        "runtime/capabilities",
        json!({"hostCapabilities": {"sessionWorkspaceRouting": true}}),
    );
    first["result"].clone()
}

/// 建会话（带第一条输入）并订阅它的 conversation 主题；返回 ACK。
fn create(host: &mut Host, session: &str, ws: &Path, text: &str) -> Value {
    let ack = host.request(
        "v4/command",
        envelope(
            &format!("create-{session}"),
            None,
            "createSession",
            json!({"sessionId": session, "workspaceId": ws.to_string_lossy(), "firstInput": {"text": text}}),
        ),
    );
    let subscribed = host.request(
        "v4/conversation/subscribe",
        json!({"topic": format!("conversation/{session}"), "connectionId": CONNECTION,
            "clientMode": "desktop-continuous", "workspace": workspace_ref(ws)}),
    );
    assert!(subscribed.get("result").is_some(), "{subscribed}");
    ack["result"].clone()
}

/// 夹具收到的、正文里有 `marker` 的那个主请求。
fn request_with(fixture: &sse::Fixture, marker: &str) -> Value {
    fixture
        .main_requests()
        .into_iter()
        .find(|request| request.to_string().contains(marker))
        .unwrap_or_else(|| panic!("no request carries {marker}"))
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn the_process_negotiates_workspace_routing() {
    let place = Place::new();
    let fixture = sse::serve(Vec::new());
    let mut host = Host::start(&place.env(&fixture.url));
    assert_eq!(host.startup()["phase"], "ready");
    let capabilities = negotiate(&mut host);
    assert_eq!(
        capabilities["sessionWorkspaceRouting"], true,
        "{capabilities}"
    );
    host.close();
}

#[test]
fn each_workspace_gets_its_own_hooks_rules_mode_and_memory() {
    let place = Place::new();
    place.configure();
    let fixture = sse::serve(Vec::new());
    let mut host = Host::start(&place.env(&fixture.url));
    assert_eq!(host.startup()["phase"], "ready");
    negotiate(&mut host);
    // 缺省模式按工作区（A 的本机项目设置是 edit；B 的项目文件写的 yolo 不生效）。
    let presentation_a = host.request(
        "workspace/readPresentation",
        json!({"workspace": workspace_ref(&place.a)}),
    );
    let presentation_b = host.request(
        "workspace/readPresentation",
        json!({"workspace": workspace_ref(&place.b)}),
    );
    assert_eq!(presentation_a["result"]["mode"], "edit", "{presentation_a}");
    assert_eq!(
        presentation_b["result"]["mode"], "build",
        "{presentation_b}"
    );
    create(&mut host, "session-a1", &place.a, "MARK_A1 hello");
    create(&mut host, "session-b1", &place.b, "MARK_B1 hello");
    create(&mut host, "session-a2", &place.a, "MARK_A2 hello");
    eventually("three model requests", || {
        fixture.main_requests().len() >= 3
    });
    let a1 = request_with(&fixture, "MARK_A1");
    let b1 = request_with(&fixture, "MARK_B1");
    // 整个 Bash 被 A 的本机项目规则集拒绝：A 的请求里没有它；B 的项目文件也拒绝它，但不生效，B 的请求里有。
    assert!(
        !tool_names(&a1).contains(&"Bash".to_owned()),
        "{:?}",
        tool_names(&a1)
    );
    assert!(
        tool_names(&b1).contains(&"Bash".to_owned()),
        "{:?}",
        tool_names(&b1)
    );
    // 记忆目录按会话所在的工作区。
    let (memory_a, memory_b) = (place.memory_dir(&place.a), place.memory_dir(&place.b));
    assert!(a1.to_string().contains(&memory_a), "{memory_a}");
    assert!(!a1.to_string().contains(&memory_b));
    assert!(b1.to_string().contains(&memory_b), "{memory_b}");
    // 项目 hooks：A 的两个会话各跑一次 A 的 hook，B 的会话只跑 B 的。
    eventually("the hooks", || place.hook_letters().len() >= 3);
    let mut letters = place.hook_letters();
    letters.sort();
    assert_eq!(letters, ["a", "a", "b"]);
    host.close();
}

#[test]
fn each_session_owns_its_mcp_connections_unless_the_server_is_shared() {
    let place = Place::new();
    place.configure();
    let fixture = sse::serve(Vec::new());
    let mut host = Host::start(&place.env(&fixture.url));
    assert_eq!(host.startup()["phase"], "ready");
    negotiate(&mut host);
    for (session, ws) in [
        ("session-a1", &place.a),
        ("session-a2", &place.a),
        ("session-b1", &place.b),
        ("session-b2", &place.b),
    ] {
        create(&mut host, session, ws, &format!("MARK_{session} hello"));
    }
    eventually("four model requests", || fixture.main_requests().len() >= 4);
    // alpha（会话级）每个会话一个进程，cwd 是会话的工作区；beta（声明 workspace）每个工作区一个。
    let named = |starts: &[(String, String, String)], wanted: &str, ws: &Path| {
        starts
            .iter()
            .filter(|(name, _, cwd)| name == wanted && Path::new(cwd) == ws)
            .cloned()
            .collect::<Vec<_>>()
    };
    eventually("the MCP servers", || {
        let starts = place.mcp_starts();
        named(&starts, "alpha", &place.a).len() >= 2
            && named(&starts, "alpha", &place.b).len() >= 2
            && !named(&starts, "beta", &place.a).is_empty()
            && !named(&starts, "beta", &place.b).is_empty()
    });
    let starts = place.mcp_starts();
    let (alpha_a, alpha_b) = (
        named(&starts, "alpha", &place.a),
        named(&starts, "alpha", &place.b),
    );
    let (beta_a, beta_b) = (
        named(&starts, "beta", &place.a),
        named(&starts, "beta", &place.b),
    );
    assert_eq!((alpha_a.len(), alpha_b.len()), (2, 2), "{starts:?}");
    assert_eq!((beta_a.len(), beta_b.len()), (1, 1), "{starts:?}");
    assert_eq!(
        starts.iter().filter(|(name, ..)| name == "alpha").count(),
        4,
        "{starts:?}"
    );
    // 项目级的声明与它们同批开始连接（B 的在组装 B 的内核时，A 的在 A 的会话打开时），一个都没起。
    assert!(
        !starts.iter().any(|(name, ..)| name.starts_with("project-")),
        "{starts:?}"
    );
    // 关掉 A1：它那一份 alpha 结束，A2 的照旧；B 的与两份 beta 不受影响。
    let closed = host.request("session/close", json!({"sessionId": "session-a1"}));
    assert_eq!(closed["result"]["closed"], true, "{closed}");
    eventually("one alpha left in A", || {
        alpha_a.iter().filter(|(_, pid, _)| alive(pid)).count() == 1
    });
    assert!(alpha_b.iter().all(|(_, pid, _)| alive(pid)), "{starts:?}");
    assert!(alive(&beta_a[0].1) && alive(&beta_b[0].1), "{starts:?}");
    host.close();
}

#[test]
fn session_requests_follow_the_registered_workspace() {
    let place = Place::new();
    let fixture = sse::serve(Vec::new());
    let mut host = Host::start(&place.env(&fixture.url));
    assert_eq!(host.startup()["phase"], "ready");
    negotiate(&mut host);
    create(&mut host, "session-a1", &place.a, "MARK_A1 hello");
    eventually("the first request", || !fixture.main_requests().is_empty());
    // 订阅时说它在 B：冲突，拒绝。
    let conflict = host.request(
        "v4/conversation/subscribe",
        json!({"topic": "conversation/session-a1", "connectionId": CONNECTION,
            "clientMode": "desktop-continuous", "workspace": workspace_ref(&place.b)}),
    );
    assert_eq!(conflict["error"]["code"], -32602, "{conflict}");
    host.close();

    // 新进程：会话在库里，但这个进程里没有登记。没登记就发命令：拒绝，不借进程 cwd 的配置冷打开。
    let mut host = Host::start(&place.env(&fixture.url));
    assert_eq!(host.startup()["phase"], "ready");
    negotiate(&mut host);
    let unrouted = host.request(
        "v4/command",
        envelope(
            "send-1",
            Some("session-a1"),
            "sendText",
            json!({"text": "MARK_UNROUTED"}),
        ),
    );
    assert_eq!(unrouted["result"]["status"], "rejected", "{unrouted}");
    assert_eq!(unrouted["result"]["reasonCode"], "proto.sessionNotFound");
    // 没有 workspace、也没有登记的订阅：拒绝。
    let unknown = host.request(
        "v4/conversation/subscribe",
        json!({"topic": "conversation/session-a1", "connectionId": CONNECTION,
            "clientMode": "desktop-continuous"}),
    );
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    // 带着工作区订阅之后可以发。
    let subscribed = host.request(
        "v4/conversation/subscribe",
        json!({"topic": "conversation/session-a1", "connectionId": CONNECTION,
            "clientMode": "desktop-continuous", "workspace": workspace_ref(&place.a)}),
    );
    assert!(subscribed.get("result").is_some(), "{subscribed}");
    let routed = host.request(
        "v4/command",
        envelope(
            "send-2",
            Some("session-a1"),
            "sendText",
            json!({"text": "MARK_ROUTED"}),
        ),
    );
    assert_eq!(routed["result"]["status"], "accepted", "{routed}");
    host.close();
}

#[test]
fn project_storage_settings_are_not_read_by_the_shared_process() {
    let place = Place::new();
    // 数据目录（进程 cwd）与工作区 A 的项目配置都想把会话库移走：共享进程只看用户级配置与环境变量。
    let moved = place.base.join("project.sqlite");
    write(
        &place.data.join(".zcode/config.json"),
        &json!({"storage": {"sessionDbPath": moved}}),
    );
    write(
        &place.a.join(".zcode/config.json"),
        &json!({"storage": {"sessionDbPath": moved}}),
    );
    let fixture = sse::serve(Vec::new());
    let mut host = Host::start(&place.env(&fixture.url));
    assert_eq!(host.startup()["phase"], "ready");
    negotiate(&mut host);
    create(&mut host, "session-a1", &place.a, "MARK_A1 hello");
    eventually("the first request", || !fixture.main_requests().is_empty());
    host.close();
    assert!(
        !moved.exists(),
        "the project storage setting moved the database"
    );
    assert!(place.home.join(".zcode/cli/db/db.sqlite").exists());
}

/// App 的交互会话照 TUI 生成标题（titles.md §1、§2）：第一条输入够长时，主请求之外还有一个标题请求（旁路请求，system 是标题提示词，
/// user 里带这条输入）。以前 app-server 的组装关着标题，只有主请求。
#[test]
fn app_sessions_ask_for_a_title_after_the_first_input() {
    let place = Place::new();
    let fixture = sse::serve(Vec::new());
    let mut host = Host::start(&place.env(&fixture.url));
    assert_eq!(host.startup()["phase"], "ready");
    negotiate(&mut host);
    create(
        &mut host,
        "session-title",
        &place.a,
        "MARK_TITLE please look at the build",
    );
    eventually("the title request", || {
        fixture
            .side_requests()
            .iter()
            .any(|request| request.to_string().contains("MARK_TITLE"))
    });
    let title = fixture
        .side_requests()
        .into_iter()
        .find(|request| request.to_string().contains("MARK_TITLE"))
        .unwrap();
    let system = title["system"].to_string();
    assert!(
        system.contains(rust_cli_prompts::title::system().lines().next().unwrap()),
        "{system}"
    );
    host.close();
}
