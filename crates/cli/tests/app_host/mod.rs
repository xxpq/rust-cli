//! 真实进程 `zcode-rs app-server --stdio` 的一个最小 Host（测试用）：一行一条 JSON，按 id 等回复；通知收进一张表；
//! 反向请求一律回 `-32601`（这些测试用 API key 的 provider，不会有取头请求）。
#![allow(dead_code, reason = "不同的测试文件用到的辅助函数不同")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::system_env::KeepSystem as _;

/// 等一个回复或一个条件的上限（存活的上限：正常不到 1 s）。
pub const WAIT: Duration = Duration::from_secs(60);

pub struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<Value>,
    next: u64,
    /// 收到的通知（方法名，参数），按到达顺序。
    pub notifications: Vec<(String, Value)>,
}

/// 起进程用的环境：临时 home、数据目录与本地的模型夹具。
pub struct Env<'a> {
    pub home: &'a Path,
    pub cwd: &'a Path,
    pub model_url: &'a str,
    pub model: &'a str,
}

impl Host {
    pub fn start(env: &Env<'_>) -> Self {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(["app-server", "--stdio"])
            .current_dir(env.cwd)
            .env_clear()
            .keep_system()
            .env("HOME", env.home)
            .env("USERPROFILE", env.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("ZCODE_DATA_BASE_DIR", env.home)
            .env("ZCODE_MODEL_BASE_URL", env.model_url)
            .env("ZCODE_API_KEY", "test-key")
            .env("ZCODE_MODEL", env.model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            lines,
            next: 0,
            notifications: Vec::new(),
        }
    }

    fn write(&mut self, value: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    }

    /// 处理一行：通知记下，反向请求回 `-32601`；是回复时交出去。
    fn take(&mut self, line: Value) -> Option<Value> {
        if line.get("method").is_some() {
            let method = line["method"].as_str().unwrap_or_default().to_owned();
            if line.get("id").is_some() {
                let reply = json!({"id": line["id"], "error": {"code": -32601, "message": "Method not found"}});
                self.write(&reply);
            } else {
                self.notifications.push((method, line["params"].clone()));
            }
            return None;
        }
        Some(line)
    }

    /// 发一个请求，等它的回复（`result` 或 `error`）。
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = format!("host-{}", self.next);
        self.write(&json!({"id": id, "method": method, "params": params}));
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no reply to {method}"));
            if let Some(reply) = self.take(line)
                && reply["id"] == id
            {
                return reply;
            }
        }
    }

    /// 读到满足条件的通知为止（已经收到的也算）。
    pub fn wait_notification(&mut self, what: &str, done: impl Fn(&str, &Value) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            if self
                .notifications
                .iter()
                .any(|(method, params)| done(method, params))
            {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            let _ = self.take(line);
        }
    }

    /// 启动帧：读到 `ready` 或 `failed`，返回最后一帧的参数。
    pub fn startup(&mut self) -> Value {
        self.wait_notification("storage state", |method, params| {
            method == "startup/storageState" && params["phase"] != "checking"
        });
        self.notifications
            .iter()
            .rev()
            .find(|(method, _)| method == "startup/storageState")
            .map(|(_, params)| params.clone())
            .unwrap()
    }

    /// 处理已经到达的行（不等）。
    pub fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            let _ = self.take(line);
        }
    }

    /// 关掉输入，等进程退出。
    pub fn close(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// 一个命令的信封（`v4/command`）。
pub fn envelope(command: &str, session: Option<&str>, kind: &str, payload: Value) -> Value {
    json!({
        "commandId": command,
        "clientId": "test-client",
        "sessionId": session,
        "type": kind,
        "payload": payload,
        "issuedAt": 1,
        "inputOrigin": "desktop",
    })
}

/// Host 的工作区引用（本地：键就是路径）。
pub fn workspace_ref(path: &Path) -> Value {
    let path = path.to_string_lossy();
    json!({"workspacePath": path, "workspaceKey": path})
}

/// 等条件成立（测试里的轮询：条件是进程外的事实，例如文件与夹具收到的请求）。
pub fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
