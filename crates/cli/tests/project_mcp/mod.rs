//! App 形态的项目级 MCP 的真实进程测试共用的部分（`app_project_mcp.rs`、`app_mcp_trust_order.rs`）：临时 home 与仓库、标记服务器、
//! 说 v5 的最小 Host（Content-Length 分帧、JSON-RPC 2.0、每帧补 1 个信用）。
#![allow(dead_code, reason = "两个测试文件用到的辅助不同")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::app_host::WAIT;
use crate::system_env::KeepSystem as _;
use rust_cli_base::js::uri::encode_uri_component;
use rust_cli_base::models::GLM_5_3;
use rust_cli_protocol_v5::generated::contract::CONTRACT_DIGEST;
use serde_json::{Value, json};

/// 订阅工作区 agent topic 用的订阅名。
pub(crate) const AGENT_SUB: &str = "agent";
/// 新会话的 ID。
pub(crate) const SESSION: &str = "ses_project_mcp";
/// 等下一条消息的一次时长：到了就再看一次进程外的条件（标记文件）。
pub(crate) const POLL: Duration = Duration::from_millis(50);

/// 五个标记服务器的名字（也是标记行里的名字）。
pub(crate) const PROJECT_SHARED: &str = "project-shared";
pub(crate) const PROJECT_SESSION: &str = "project-session";
pub(crate) const USER_SHARED: &str = "user-shared";
pub(crate) const USER_SESSION: &str = "user-session";
pub(crate) const PLUGIN: &str = "plugin";
/// 仓库里的插件的标记服务器。
pub(crate) const REPO_PLUGIN: &str = "repo-plugin";
/// 两个插件的 SessionStart hook 在标记日志里记的名字（`hook <名字>`）。
pub(crate) const USER_HOOK: &str = "user-plugin-hook";
pub(crate) const REPO_HOOK: &str = "repo-plugin-hook";
/// 仓库里的插件目录（项目配置里写的相对路径，相对仓库根）。
pub(crate) const REPO_PLUGIN_DIR: &str = "tools/kit";
/// 改过声明之后会话级的那个服务器在标记日志里的名字（服务器名仍是 `project-session`）。
pub(crate) const PROJECT_SESSION_V2: &str = "project-session-v2";
/// `mcp.trust.set` 的命令 ID 里取 digest 的前几位（同一个服务器的不同声明是不同的命令）。
pub(crate) const DIGEST_ID_CHARS: usize = 8;
/// 仓库的项目配置（相对仓库根）。
pub(crate) const PROJECT_CONFIG: &str = ".zcode/config.json";
/// 项目配置里服务器表的位置。
pub(crate) const PROJECT_TABLE: [&str; 2] = ["mcp", "servers"];
/// 回落文件（相对仓库根，18-mcp-trust.md §14）与它的服务器表。
pub(crate) const AGENTS_MCP: &str = ".agents/mcp.json";
pub(crate) const AGENTS_TABLE: [&str; 1] = ["mcpServers"];
/// 回落文件里的三个标记服务器：内核级的、每会话的、一个不合法的邻项（`timeoutMs: 0`，Node 与 rust 都不收；收了它就能起）。
pub(crate) const AGENTS_SHARED: &str = "agents-shared";
pub(crate) const AGENTS_SESSION: &str = "agents-session";
pub(crate) const AGENTS_BROKEN: &str = "agents-broken";
/// 用户级的回落文件（相对 home，18-mcp-trust.md §14.10）。
pub(crate) const USER_AGENTS_MCP: &str = ".agents/mcp.json";
/// 用户级回落文件里的标记服务器：只在那里的一个（内核级）；与仓库里的 `project-shared` 同名的那一个在标记日志里的名字。
pub(crate) const USER_AGENTS: &str = "user-agents";
pub(crate) const USER_AGENTS_COPY: &str = "user-agents-copy";
/// 工作区 MCP 的信任记录（相对 home，存储根没改时，18 §12.3）。
pub(crate) const TRUST_STORE: &str = ".zcode/security/workspace-mcp-trust-v1.json";

/// 测试进程的 PATH 里的 node（`pnpm test:rust-cli` 由 node 启动，PATH 里一定有）。
pub(crate) fn node() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
        .expect("node is not on PATH")
}

pub(crate) fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value.to_string()).unwrap();
}

/// 临时目录：home、数据目录（进程 cwd）、仓库、插件目录与标记日志。
pub(crate) struct Place {
    pub(crate) _dir: tempfile::TempDir,
    pub(crate) home: PathBuf,
    pub(crate) data: PathBuf,
    pub(crate) repo: PathBuf,
    pub(crate) plugin: PathBuf,
    /// 标记服务器的起停记录（`tests/fixtures/mcp-counter.mjs`：每起一个进程记一行 `start <名字> <pid> <cwd>`）；插件的
    /// hook 每跑一次也在这里记一行 `hook <名字>`。
    pub(crate) mcp_log: PathBuf,
}

impl Place {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let place = Self {
            home: base.join("home"),
            data: base.join("data"),
            repo: base.join("repo"),
            plugin: base.join("plugin"),
            mcp_log: base.join("mcp.log"),
            _dir: dir,
        };
        for dir in [&place.home, &place.data] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(place.repo.join(".git")).unwrap();
        place
    }

    /// 一个标记服务器（`isolation` 为 None 时不写这个字段）。
    pub(crate) fn server(&self, name: &str, isolation: Option<&str>) -> Value {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-counter.mjs");
        let mut server =
            json!({"type": "stdio", "command": node(), "args": [script, self.mcp_log, name]});
        if let Some(isolation) = isolation {
            server["isolation"] = json!(isolation);
        }
        server
    }

    /// 仓库里的项目配置（不受信任的来源）、用户级配置与它引入的插件（用户自己装的来源）。
    pub(crate) fn configure(&self) {
        write(
            &self.repo.join(".zcode/config.json"),
            &json!({"mcp": {"servers": {
                PROJECT_SHARED: self.server(PROJECT_SHARED, Some("workspace")),
                PROJECT_SESSION: self.server(PROJECT_SESSION, None),
            }}}),
        );
        write(
            &self.home.join(".zcode/cli/config.json"),
            &json!({
                "mcp": {"servers": {
                    USER_SHARED: self.server(USER_SHARED, Some("workspace")),
                    USER_SESSION: self.server(USER_SESSION, None),
                }},
                "plugins": {"dirs": [self.plugin]},
            }),
        );
        write(
            &self.plugin.join(".zcode-plugin/plugin.json"),
            &json!({"name": "kit"}),
        );
        write(
            &self.plugin.join(".mcp.json"),
            &json!({"mcpServers": {"probe": self.server(PLUGIN, None)}}),
        );
    }

    /// 信任一节（18 §12.10）的配置：仓库声明两个项目级服务器，`project-session` 的声明可以换成第二版；用户级两个作对照。
    pub(crate) fn configure_trust(&self, session_v2: bool) {
        let session = if session_v2 {
            self.server(PROJECT_SESSION_V2, None)
        } else {
            self.server(PROJECT_SESSION, None)
        };
        write(
            &self.repo.join(PROJECT_CONFIG),
            &json!({"mcp": {"servers": {
                PROJECT_SHARED: self.server(PROJECT_SHARED, Some("workspace")),
                PROJECT_SESSION: session,
            }}}),
        );
        write(
            &self.home.join(".zcode/cli/config.json"),
            &json!({"mcp": {"servers": {
                USER_SHARED: self.server(USER_SHARED, Some("workspace")),
                USER_SESSION: self.server(USER_SESSION, None),
            }}}),
        );
    }

    /// 仓库只有回落文件 `.agents/mcp.json`（没有项目配置）：内核级、每会话的各一个，加一个不合法的邻项；用户级两个作对照。
    pub(crate) fn configure_agents(&self) {
        let mut broken = self.server(AGENTS_BROKEN, Some("workspace"));
        broken["timeoutMs"] = json!(0);
        write(
            &self.repo.join(AGENTS_MCP),
            &json!({"mcpServers": {
                AGENTS_SHARED: self.server(AGENTS_SHARED, Some("workspace")),
                AGENTS_SESSION: self.server(AGENTS_SESSION, None),
                AGENTS_BROKEN: broken,
            }}),
        );
        write(
            &self.home.join(".zcode/cli/config.json"),
            &json!({"mcp": {"servers": {
                USER_SHARED: self.server(USER_SHARED, Some("workspace")),
                USER_SESSION: self.server(USER_SESSION, None),
            }}}),
        );
    }

    /// 用户只有 `~/.agents/mcp.json`（没有 `~/.zcode/cli/config.json`）：一个只在那里的内核级服务器，一个与仓库里的 `project-shared`
    /// 同名（标记名不同，起了就看得出是哪一个）。仓库的项目配置声明 `project-shared`（不受信任的来源）。
    pub(crate) fn configure_user_agents(&self) {
        write(
            &self.home.join(USER_AGENTS_MCP),
            &json!({"mcpServers": {
                USER_AGENTS: self.server(USER_AGENTS, Some("workspace")),
                PROJECT_SHARED: self.server(USER_AGENTS_COPY, Some("workspace")),
            }}),
        );
        write(
            &self.repo.join(PROJECT_CONFIG),
            &json!({"mcp": {"servers": {
                PROJECT_SHARED: self.server(PROJECT_SHARED, Some("workspace")),
            }}}),
        );
    }

    /// 仓库的项目配置里这个服务器现在那份声明的 digest（[`Self::digest_in`]）。
    pub(crate) fn digest(&self, name: &str) -> String {
        self.digest_in(PROJECT_CONFIG, &PROJECT_TABLE, name)
    }

    /// 回落文件里这个服务器的 digest（[`Self::digest_in`]）。
    pub(crate) fn agents_digest(&self, name: &str) -> String {
        self.digest_in(AGENTS_MCP, &AGENTS_TABLE, name)
    }

    /// 仓库里 `file` 的服务器表 `table` 里这个服务器现在那份声明的 digest：照 Node `workspace-mcp-trust` 的写法另算一遍（与 rust 的
    /// 实现无关的对照，18 §12.2）：按键排序的 `{version, workspaceIdentity, workingDirectory, serverName, sourcePaths, config}` 的 sha256；
    /// stdio 的 `config` 补 `env: {}` 与 `cwd`（两种文件里没写 `cwd` 时都是仓库根：项目配置在 `.zcode/` 下，回落文件按工作区根）；
    /// `sourcePaths` 只有这一个文件（仓库里只有它）。
    pub(crate) fn digest_in(&self, file: &str, table: &[&str], name: &str) -> String {
        let text = std::fs::read_to_string(self.repo.join(file)).unwrap();
        let declared: Value = serde_json::from_str(&text).unwrap();
        let mut config = table.iter().fold(&declared, |value, key| &value[*key])[name].clone();
        config["env"] = json!({});
        config["cwd"] = json!(self.repo);
        let payload = json!({
            "version": 1,
            "workspaceIdentity": self.repo,
            "workingDirectory": self.repo,
            "serverName": name,
            "sourcePaths": [self.repo.join(file)],
            "config": config,
        });
        rust_cli_base::hash::sha256_hex(
            rust_cli_base::js::json::stringify(&sorted(&payload)).as_bytes(),
        )
    }

    pub(crate) fn trust_store(&self) -> PathBuf {
        self.home.join(TRUST_STORE)
    }

    /// 起过的标记服务器的名字（每起一个进程一项）。
    pub(crate) fn started(&self) -> Vec<String> {
        std::fs::read_to_string(&self.mcp_log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("start "))
            .filter_map(|rest| rest.split(' ').next())
            .map(str::to_owned)
            .collect()
    }

    pub(crate) fn starts_of(&self, name: &str) -> usize {
        self.started()
            .iter()
            .filter(|started| *started == name)
            .count()
    }

    /// 名为 `name` 的 hook 跑过几次（每次一行 `hook <名字>`）。
    pub(crate) fn hooks_of(&self, name: &str) -> usize {
        std::fs::read_to_string(&self.mcp_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.strip_prefix("hook ") == Some(name))
            .count()
    }

    /// 一个插件：manifest、一个标记服务器、一个 SessionStart 时在标记日志里记 `hook <hook>` 的 hook。
    pub(crate) fn write_plugin(&self, dir: &Path, name: &str, server: &str, hook: &str) {
        write(
            &dir.join(".zcode-plugin/plugin.json"),
            &json!({"name": name}),
        );
        write(
            &dir.join(".mcp.json"),
            &json!({"mcpServers": {"probe": self.server(server, None)}}),
        );
        let command = format!("echo 'hook {hook}' >> '{}'", self.mcp_log.display());
        write(
            &dir.join("hooks/hooks.json"),
            &json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": command}]}]}}),
        );
    }

    /// 仓库的项目配置只用 `plugins.dirs` 引入仓库里的一个插件（不受信任的来源）；用户级配置引入用户自己的插件（对照）。
    pub(crate) fn configure_plugin_dirs(&self) {
        write(
            &self.repo.join(".zcode/config.json"),
            &json!({"plugins": {"dirs": [format!("./{REPO_PLUGIN_DIR}")]}}),
        );
        write(
            &self.home.join(".zcode/cli/config.json"),
            &json!({"plugins": {"dirs": [self.plugin]}}),
        );
        self.write_plugin(&self.plugin, "kit", PLUGIN, USER_HOOK);
        self.write_plugin(
            &self.repo.join(REPO_PLUGIN_DIR),
            "repo-kit",
            REPO_PLUGIN,
            REPO_HOOK,
        );
    }
}

/// 说 v5 的最小 Host：Content-Length 分帧、JSON-RPC 2.0；每收到一帧补 1 个信用（01 §4.3）。
pub(crate) struct V5Host {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: mpsc::Receiver<Value>,
    next: u64,
    /// 收到的帧（`frame` 通知的参数）。
    frames: Vec<Value>,
}

impl V5Host {
    pub(crate) fn start(place: &Place, model_url: &str) -> Self {
        let mut child = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
            .args(["app-server", "--stdio", "--protocol", "v5"])
            .current_dir(&place.data)
            .env_clear()
            .keep_system()
            .env("HOME", &place.home)
            .env("USERPROFILE", &place.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("ZCODE_DATA_BASE_DIR", &place.home)
            .env("ZCODE_MODEL_BASE_URL", model_url)
            .env("ZCODE_API_KEY", "test-key")
            .env("ZCODE_MODEL", GLM_5_3)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = read_frame(&mut reader) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            messages,
            next: 0,
            frames: Vec::new(),
        }
    }

    pub(crate) fn send(&mut self, message: &Value) {
        let body = message.to_string();
        let stdin = self.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        stdin.flush().unwrap();
    }

    /// 帧记下并补信用；是回复时交出去。
    pub(crate) fn take(&mut self, message: Value) -> Option<Value> {
        if message["method"] == "frame" {
            let sub = message["params"]["sub"].clone();
            self.send(
                &json!({"jsonrpc": "2.0", "method": "credit", "params": {"sub": sub, "n": 1}}),
            );
            self.frames.push(message["params"].clone());
            return None;
        }
        message.get("method").is_none().then_some(message)
    }

    pub(crate) fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let message = self
                .messages
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no reply to {method}"));
            if let Some(reply) = self.take(message)
                && reply["id"] == id
            {
                assert!(reply.get("error").is_none(), "{method}: {reply}");
                return reply["result"].clone();
            }
        }
    }

    /// 严格链路的握手（带契约指纹）。
    pub(crate) fn initialize(&mut self) {
        self.request(
            "initialize",
            json!({
                "clientId": "test-host",
                "peer": {"kind": "host", "name": "test-host", "build": {"version": "test"}},
                "caps": {"core": [1], "agent": [1], "session": [1], "mcp.trust": [1]},
                "require": ["core", "agent", "session"],
                "contract": CONTRACT_DIGEST,
            }),
        );
    }

    /// 订工作区的 agent topic，等到 `phase: ready`：组装这个仓库的内核（加载插件、开始连内核级的服务器）。
    pub(crate) fn open_workspace(&mut self, repo: &str) {
        self.request(
            "subscribe",
            json!({"sub": AGENT_SUB, "topic": format!("ws/{}/agent", encode_uri_component(repo))}),
        );
        self.wait_until("the workspace kernel", |host| {
            host.phase_is(AGENT_SUB, "ready")
        });
    }

    /// 建会话（不带输入）：会话 actor 启动，开始连每会话的服务器（插件的服务器也是每会话的）、跑 SessionStart hook。
    pub(crate) fn create_session(&mut self, repo: &str) {
        self.create_session_as(repo, SESSION, "019a5f3c-7a2b-7c3d-8e4f-0000000000a1");
    }

    /// 同上，会话 ID 与命令 ID（UUIDv7）由调用方给。
    pub(crate) fn create_session_as(&mut self, repo: &str, session: &str, command: &str) {
        let ack = self.request(
            "command",
            json!({
                "commandId": command,
                "type": "session.create",
                "target": {"hostId": "local", "workspaceKey": repo},
                "payload": {"sessionId": session},
            }),
        );
        assert_eq!(ack["kind"], "accepted", "{ack}");
    }

    /// `mcp.trust.set`（18 §12.5）：返回命令的结果。
    pub(crate) fn trust(&mut self, repo: &str, server: &str, digest: &str, trusted: bool) -> Value {
        self.request("command", trust_command(repo, server, digest, trusted))
    }

    /// 发一个请求，不等回复：返回它的 id。
    pub(crate) fn begin(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// 下一个到的回复（中间的帧照常记下、补信用）。
    pub(crate) fn next_reply(&mut self) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let message = self.messages.recv_timeout(left).expect("no reply");
            if let Some(reply) = self.take(message) {
                return reply;
            }
        }
    }

    /// `agent.release` 之后用新的订阅名再订工作区的 agent topic、等到 ready：内核按磁盘上的配置重新组装（18 §12.4）。
    pub(crate) fn reassemble(&mut self, repo: &str, sub: &str) {
        let ack = self.request(
            "command",
            json!({
                "commandId": format!("release-{sub}"),
                "type": "agent.release",
                "target": {"hostId": "local", "workspaceKey": repo},
                "payload": {},
            }),
        );
        assert_eq!(ack["kind"], "accepted", "{ack}");
        self.request(
            "subscribe",
            json!({"sub": sub, "topic": format!("ws/{}/agent", encode_uri_component(repo))}),
        );
        self.wait_until("the reassembled workspace kernel", |host| {
            host.phase_is(sub, "ready")
        });
    }

    /// 订一个会话的 topic（拿它的快照与之后的帧）。
    pub(crate) fn watch_session(&mut self, repo: &str, session: &str, sub: &str) {
        self.request(
            "subscribe",
            json!({"sub": sub, "topic": format!("ws/{}/session/{session}", encode_uri_component(repo))}),
        );
    }

    /// 收到的帧里有没有这段文字（敏感值不该出现在任何帧里）。
    pub(crate) fn frames_contain(&self, text: &str) -> bool {
        self.frames
            .iter()
            .any(|frame| frame.to_string().contains(text))
    }

    /// 订阅 `sub` 收到的帧里 state 的 `mcpAdmission` 依次的值（快照或增量的 `state.set`）。
    pub(crate) fn mcp_admissions(&self, sub: &str) -> Vec<Value> {
        let mut values = Vec::new();
        for frame in self.frames.iter().filter(|frame| frame["sub"] == sub) {
            let body = &frame["body"];
            if let Some(value) = body["snapshot"]["state"].get("mcpAdmission") {
                values.push(value.clone());
            }
            for delta in body["deltas"].as_array().into_iter().flatten() {
                if delta["op"] == "state.set"
                    && let Some(value) = delta["patch"].get("mcpAdmission")
                {
                    values.push(value.clone());
                }
            }
        }
        values
    }

    /// 同上，最后的值。
    pub(crate) fn mcp_admission(&self, sub: &str) -> Option<Value> {
        self.mcp_admissions(sub).pop()
    }

    /// 一边收消息（补信用）一边等条件成立；条件看进程外的事实（标记文件）或收到的帧。
    pub(crate) fn wait_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            if let Ok(message) = self.messages.recv_timeout(POLL) {
                let _ = self.take(message);
            }
        }
    }

    /// 订阅 `sub` 收到的帧里，state 的 `phase` 最后是不是 `phase`（快照的 state 或增量的 `state.set`）。
    pub(crate) fn phase_is(&self, sub: &str, phase: &str) -> bool {
        let mut last = None;
        for frame in self.frames.iter().filter(|frame| frame["sub"] == sub) {
            let body = &frame["body"];
            if let Some(value) = body["snapshot"]["state"].get("phase") {
                last = Some(value.clone());
            }
            for delta in body["deltas"].as_array().into_iter().flatten() {
                if delta["op"] == "state.set"
                    && let Some(value) = delta["patch"].get("phase")
                {
                    last = Some(value.clone());
                }
            }
        }
        last.is_some_and(|value| value == phase)
    }

    /// 关掉输入，等进程退出（它关掉的 MCP 进程在标记日志里留下了起过的记录）。
    pub(crate) fn close(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// `mcp.trust.set` 的命令信封（命令 ID 带 digest 的前几位：同一个服务器的不同声明是不同的命令）。
pub(crate) fn trust_command(repo: &str, server: &str, digest: &str, trusted: bool) -> Value {
    json!({
        "commandId": format!("trust-{server}-{}-{trusted}", &digest[..DIGEST_ID_CHARS]),
        "type": "mcp.trust.set",
        "target": {"hostId": "local", "workspaceKey": repo},
        "payload": {"serverName": server, "declarationDigest": digest, "trusted": trusted},
    })
}

/// 对象的键排好序（这里的键都是 ASCII，字节序就是 JS `Array.prototype.sort` 的码元序）。
pub(crate) fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let ordered: std::collections::BTreeMap<&String, Value> =
                map.iter().map(|(key, item)| (key, sorted(item))).collect();
            Value::Object(
                ordered
                    .into_iter()
                    .map(|(key, item)| (key.clone(), item))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// 读一帧（只认 `Content-Length` 头）；输出结束时为 None。
pub(crate) fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}
