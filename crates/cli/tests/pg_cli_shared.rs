//! CLI 内部测试的共用 PG（docs/specs/rust-cli/pg-store.md §11）：真实的 `zcode-rs -p` 进程、本地模型替身与一个专用的
//! 测试库，模拟几位测试者配好同样的部署配置（`store.pg`）按 userId 隔离数据。默认 ignored：需要
//! `ZCODE_E2E_SESSION_PG_URL`（业务角色，库已迁移）；每次用新的用户 ID，不清理库里的数据。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读测试库的连接串"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use system_env::KeepSystem as _;

use serde_json::Value;

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 本地模型替身：回固定的文本，记下带 tools 的主请求。
fn model_server() -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            if let Ok(value) = serde_json::from_slice::<Value>(&body)
                && value.get("tools").is_some()
            {
                seen.lock().unwrap().push(value);
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                REPLY.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(REPLY.as_bytes());
        }
    });
    (format!("http://{address}"), bodies)
}

/// 放连接串的环境变量：部署配置只写这个名字（deployment.md §6）。
const URL_ENV: &str = "ZCODE_TEST_SHARED_PG_URL";

/// 一位测试者：自己的 HOME（本机配置、日志），共用的库连接与独立的用户身份。
struct Tester {
    home: PathBuf,
    name: &'static str,
    user_id: String,
}

impl Tester {
    fn new(base: &Path, name: &'static str, user_id: &str) -> Self {
        let home = base.join(name);
        std::fs::create_dir_all(&home).unwrap();
        Self {
            home,
            name,
            user_id: user_id.to_owned(),
        }
    }

    /// 写这次运行的部署配置，返回它的路径。
    fn deployment(&self, user_id: bool) -> PathBuf {
        let mut pg = serde_json::json!({"url_env": URL_ENV});
        if user_id {
            pg["user_id"] = self.user_id.clone().into();
        }
        let path = self.home.join("deployment.json");
        std::fs::write(&path, serde_json::json!({"store": {"pg": pg}}).to_string()).unwrap();
        path
    }

    fn run(&self, model: &str, work: &Path, args: &[&str], user_id: bool) -> Output {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(["--output-format", "json"])
            .args(args)
            .current_dir(work)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .env("ZCODE_MODEL_BASE_URL", model)
            .env("ZCODE_API_KEY", "test-key")
            .env("ZCODE_MODEL", rust_cli_base::models::GLM_5_3)
            .env("ZCODE_DEPLOYMENT", self.deployment(user_id))
            .env(URL_ENV, std::env::var("ZCODE_E2E_SESSION_PG_URL").unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.output().unwrap()
    }

    /// 成功的一次 `-p`，返回会话 ID。
    fn ask(&self, model: &str, work: &Path, args: &[&str]) -> String {
        let output = self.run(model, work, args, true);
        assert!(
            output.status.success(),
            "{}: {}",
            self.name,
            String::from_utf8_lossy(&output.stderr)
        );
        // 会话不在本机库：没有回退到 SQLite。
        assert!(
            !self.home.join(".zcode/cli/db/db.sqlite").exists(),
            "{}",
            self.name
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        result["session_id"].as_str().unwrap().to_owned()
    }
}

/// 最后一个主请求里出现的用户文本。
fn last_request(bodies: &Mutex<Vec<Value>>) -> String {
    bodies.lock().unwrap().last().unwrap()["messages"].to_string()
}

#[test]
#[ignore = "needs ZCODE_E2E_SESSION_PG_URL for a dedicated, migrated test database"]
fn same_user_resumes_across_processes_and_other_users_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (shared, other) = (base.join("shared-repo"), base.join("other-repo"));
    for path in [&shared, &other] {
        std::fs::create_dir_all(path.join(".git")).unwrap();
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let user_id = format!("e2e-{}-{nanos}", std::process::id());
    let alice = Tester::new(&base, "alice", &user_id);
    let second = Tester::new(&base, "second-machine", &user_id);
    let carol = Tester::new(&base, "carol", &user_id);
    let bob = Tester::new(&base, "bob", &format!("{user_id}-other"));
    let (model, bodies) = model_server();

    // 同一个用户同时在不同工作区开新会话。
    let (first, carols) = std::thread::scope(|scope| {
        let a = scope.spawn(|| alice.ask(&model, &shared, &["-p", "alpha-from-alice"]));
        let c = scope.spawn(|| carol.ask(&model, &other, &["-p", "gamma-from-carol"]));
        (a.join().unwrap(), c.join().unwrap())
    });
    assert_ne!(first, carols);

    // 同一用户另一台机器、同一路径：second `-c` 续上 alice 的会话，模型看到 alice 的输入，看不到 carol 的。
    let continued = second.ask(&model, &shared, &["-c", "-p", "beta-from-second-machine"]);
    assert_eq!(continued, first);
    let seen = last_request(&bodies);
    assert!(
        seen.contains("alpha-from-alice") && seen.contains("beta-from-second-machine"),
        "{seen}"
    );
    assert!(!seen.contains("gamma-from-carol"), "{seen}");

    // 另一路径只看自己的项目：carol `-c` 续上自己的会话。
    let own = carol.ask(&model, &other, &["-c", "-p", "delta-from-carol"]);
    assert_eq!(own, carols);
    let seen = last_request(&bodies);
    assert!(
        seen.contains("gamma-from-carol") && !seen.contains("alpha-from-alice"),
        "{seen}"
    );

    // 另一个用户：同一个会话 ID 找不到。
    let foreign = bob.run(&model, &shared, &["--resume", &first, "-p", "steal"], true);
    assert!(!foreign.status.success());
    assert!(
        String::from_utf8_lossy(&foreign.stderr).contains("No conversation found"),
        "{}",
        String::from_utf8_lossy(&foreign.stderr)
    );
    assert!(!last_request(&bodies).contains("steal"));

    let wrong_workspace = second.run(
        &model,
        &other,
        &["--resume", &first, "-p", "wrong-workspace"],
        true,
    );
    assert!(!wrong_workspace.status.success());
    assert!(String::from_utf8_lossy(&wrong_workspace.stderr).contains("No conversation found"));
    assert!(!last_request(&bodies).contains("wrong-workspace"));

    // 缺 user_id：报错退出，不建本机库。
    let missing = alice.run(&model, &shared, &["-p", "no-user_id"], false);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("missing field `user_id`"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(!alice.home.join(".zcode/cli/db/db.sqlite").exists());
}
