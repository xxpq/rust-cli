//! serve 的真实进程（docs/specs/rust-cli/multi-tenant.md §4、headless.md §8）：本地假的 Anthropic 端点，`zcode-rs serve`
//! 在本机端口上监听，测试扮演运行壳经 WebSocket 发握手与 user 消息、收 stream-json。
//!
//! 只给测试的 `_ZCODE_ASSUME_SERVE_LOCAL_PRIMARY` 让这一轮在 serve 本机这个用户的目录里执行（产品的 serve 只接中转）；
//! 不配 PG 时每个用户一个本机库。PG 的版本在最后，跑 `scripts/rust-cli/pg-store.mjs` 时才跑（`--ignored`）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：起本地 HTTP 夹具与 serve 进程、读它的 stdout"
)]

mod system_env;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rust_cli_base::models::GLM_5_3;
use rust_cli_base::secret::RedactedString;
use rust_cli_net::websocket::{WebSocket, WsFrame, WsReader, WsWriter};
use rust_cli_net::{Egress, HeaderValue, NetworkPolicy};
use serde_json::{Value, json};
use system_env::KeepSystem as _;

const KEY: &str = "test-key-serve-7c21";
const TOKEN: &str = "serve-token-0b9e";
const UUID: &str = "2f6b8c1d-4e3a-4b5c-9d7e-1a2b3c4d5e6f";
const MAX: u64 = 4 * 1024 * 1024;

/// 一段文本回复。
const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_s_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":7,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 假的模型端点：每个连接一个线程，记下请求体；`hold` 时回复要等 [`Model::release`]。
struct Model {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Model {
    fn start(hold: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new((Mutex::new(!hold), Condvar::new()));
        let (recorded, opened) = (bodies.clone(), gate.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (recorded, opened) = (recorded.clone(), opened.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0; length];
                    let _ = reader.read_exact(&mut body);
                    if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                        recorded.lock().unwrap().push(value);
                    }
                    let (open, wake) = &*opened;
                    let mut ready = open.lock().unwrap();
                    while !*ready {
                        ready = wake.wait(ready).unwrap();
                    }
                    drop(ready);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nrequest-id: req_s\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        REPLY.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(REPLY.as_bytes());
                });
            }
        });
        Self { url, bodies, gate }
    }

    fn release(&self) {
        let (open, wake) = &*self.gate;
        *open.lock().unwrap() = true;
        wake.notify_all();
    }

    /// 主回合的请求（带 messages 的）。
    fn turns(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body["messages"].is_array())
            .cloned()
            .collect()
    }
}

/// 一个 serve 进程。丢掉时杀掉。
struct Serve {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Serve {
    fn start(model: &Model, extra: &[&str], env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--token-env",
                "SERVE_TOKEN",
            ])
            .args(["--data-dir", &base.join("data").to_string_lossy()])
            .args(extra)
            .current_dir(&base)
            .env_clear()
            .keep_system()
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("SERVE_TOKEN", TOKEN)
            .env("ZCODE_MODEL_BASE_URL", &model.url)
            .env("ZCODE_API_KEY", KEY)
            .env("ZCODE_MODEL", GLM_5_3)
            .env("_ZCODE_ASSUME_SERVE_LOCAL_PRIMARY", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(base.join("serve.stderr")).unwrap());
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        // 起不来时 stdout 是空的：把 stderr 带进失败信息（以前丢掉 stderr，只看得到 JSON 解析到结尾）。
        let ready: Value = serde_json::from_str(&ready).unwrap_or_else(|_| {
            let _ = child.wait();
            panic!(
                "serve did not start: {}",
                std::fs::read_to_string(base.join("serve.stderr")).unwrap_or_default()
            )
        });
        assert_eq!(ready["type"], "serve_ready");
        Self {
            child,
            address: ready["listen"].as_str().unwrap().to_owned(),
            _dir: dir,
        }
    }

    async fn connect(&self, token: &str) -> Result<(WsReader, WsWriter), String> {
        let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
        let headers = vec![(
            "authorization".to_owned(),
            HeaderValue::Secret(RedactedString::new(format!("Bearer {token}"))),
        )];
        egress
            .websocket(&format!("ws://{}/", self.address), (&[], &headers), MAX)
            .await
            .map(WebSocket::split)
            .map_err(|error| error.to_string())
    }
}

fn open(user: &str, turn: &str, session: Value) -> String {
    json!({"type": "session_open", "user": user, "turn_id": turn, "session": session, "permission_mode": "bypassPermissions"})
        .to_string()
}

fn user_line(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
}

/// 读到连接关闭为止的全部文本消息。
async fn collect(reader: &mut WsReader) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Ok(read) = tokio::time::timeout(Duration::from_secs(30), reader.next())
        .await
        .unwrap()
    {
        match read {
            WsFrame::Text(text) => lines.push(serde_json::from_str(&text).unwrap()),
            WsFrame::Close { .. } => break,
            WsFrame::Binary(_) => {}
        }
    }
    lines
}

/// 读到下一条文本消息。
async fn next(reader: &mut WsReader) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(30), reader.next())
            .await
            .unwrap()
            .unwrap()
        {
            WsFrame::Text(text) => return serde_json::from_str(&text).unwrap(),
            WsFrame::Close { code, reason } => panic!("closed: {code} {reason}"),
            WsFrame::Binary(_) => {}
        }
    }
}

/// 跑一轮，交出全部输出。
async fn turn(serve: &Serve, user: &str, turn_id: &str, session: Value, text: &str) -> Vec<Value> {
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(&open(user, turn_id, session))
        .await
        .unwrap();
    writer.send_text(&user_line(text)).await.unwrap();
    collect(&mut reader).await
}

fn result(lines: &[Value]) -> &Value {
    lines
        .iter()
        .find(|line| line["type"] == "result")
        .unwrap_or_else(|| panic!("no result in {lines:?}"))
}

/// 两个用户同时用同一个会话 ID 各开一个会话；续用时带着上一轮的历史；别的用户续用不到。每行带 `turn_id` 与连续的 `seq`。
#[tokio::test]
async fn users_share_a_process_but_not_sessions() {
    let model = Model::start(false);
    let serve = Serve::start(&model, &[], &[]);
    let (alice, bob) = tokio::join!(
        turn(
            &serve,
            "alice",
            "t-1",
            json!({"new": UUID}),
            "hello from alice"
        ),
        turn(&serve, "bob", "t-1", json!({"new": UUID}), "hello from bob"),
    );
    for lines in [&alice, &bob] {
        assert_eq!(result(lines)["subtype"], "success", "{lines:?}");
        assert_eq!(lines[0]["type"], "system", "{lines:?}");
        assert_eq!(lines[0]["session_id"], format!("sess_{UUID}"));
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(line["turn_id"], "t-1");
            assert_eq!(line["seq"], json!(index + 1));
        }
    }
    let resumed = turn(&serve, "alice", "t-2", json!({"resume": UUID}), "again").await;
    assert_eq!(result(&resumed)["subtype"], "success", "{resumed:?}");
    let last = model.turns().last().cloned().unwrap();
    let history = serde_json::to_string(&last["messages"]).unwrap();
    assert!(
        history.contains("hello from alice") && history.contains("again"),
        "{history}"
    );
    assert!(!history.contains("hello from bob"), "{history}");
    // carol 的库里没有这个会话：一个错误帧，然后关连接。
    let missing = turn(&serve, "carol", "t-1", json!({"resume": UUID}), "hi").await;
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!(missing[0]["type"], "session_error");
    assert_eq!(missing[0]["code"], "session_not_found");
}

#[tokio::test]
async fn a_wrong_token_and_bad_handshakes_are_refused() {
    let model = Model::start(false);
    let serve = Serve::start(&model, &[], &[]);
    let Err(refused) = serve.connect("wrong").await else {
        panic!("a wrong token should be refused");
    };
    assert!(refused.contains("401"), "{refused}");
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(&json!({"type": "session_open", "user": "u", "turn_id": "t", "session": {"new": "not-a-uuid"}}).to_string())
        .await
        .unwrap();
    let lines = collect(&mut reader).await;
    assert_eq!(lines[0]["code"], "bad_request", "{lines:?}");
    // 同一个用户的同一个 turn_id 已经在跑。
    let slow = Model::start(true);
    let serve = Serve::start(&slow, &[], &[]);
    let (mut first, mut first_writer) = serve.connect(TOKEN).await.unwrap();
    first_writer
        .send_text(&open("u", "t", json!({"new": UUID})))
        .await
        .unwrap();
    first_writer.send_text(&user_line("hi")).await.unwrap();
    assert_eq!(next(&mut first).await["type"], "system");
    let again = turn(&serve, "u", "t", json!({"new": UUID}), "hi").await;
    assert_eq!(again[0]["code"], "turn_exists", "{again:?}");
    slow.release();
    assert_eq!(result(&collect(&mut first).await)["subtype"], "success");
}

/// 一轮之中打断：回执，然后这一轮按中断结束（同 `-p` 收到 SIGINT）。
#[tokio::test]
async fn an_interrupt_ends_the_turn() {
    let model = Model::start(true);
    let serve = Serve::start(&model, &[], &[]);
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(&open("u", "t", json!({"new": UUID})))
        .await
        .unwrap();
    writer.send_text(&user_line("wait")).await.unwrap();
    assert_eq!(next(&mut reader).await["type"], "system");
    let deadline = Instant::now() + Duration::from_secs(10);
    while model.turns().is_empty() {
        assert!(Instant::now() < deadline, "the model request never came");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    writer
        .send_text(&json!({"type": "control_request", "request_id": "r-1", "request": {"subtype": "interrupt"}}).to_string())
        .await
        .unwrap();
    let lines = collect(&mut reader).await;
    assert!(
        lines
            .iter()
            .any(|line| line["type"] == "control_response"
                && line["response"]["request_id"] == "r-1"),
        "{lines:?}"
    );
    result(&lines);
    model.release();
}

/// 一轮之中断开：这一轮继续跑；宽限内用 `session_attach` 从断点接着收，直到 result。
#[tokio::test]
async fn a_dropped_connection_can_reattach_within_the_grace() {
    let model = Model::start(true);
    let serve = Serve::start(&model, &["--grace", "30"], &[]);
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(&open("u", "t", json!({"new": UUID})))
        .await
        .unwrap();
    writer.send_text(&user_line("wait")).await.unwrap();
    let init = next(&mut reader).await;
    assert_eq!(init["seq"], 1);
    drop((reader, writer));
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(
            &json!({"type": "session_attach", "user": "u", "turn_id": "t", "after_seq": 1})
                .to_string(),
        )
        .await
        .unwrap();
    model.release();
    let lines = collect(&mut reader).await;
    assert_eq!(lines[0]["seq"], 2, "{lines:?}");
    assert_eq!(result(&lines)["subtype"], "success");
    // 交完之后这一轮不在了。
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(&json!({"type": "session_attach", "user": "u", "turn_id": "t"}).to_string())
        .await
        .unwrap();
    assert_eq!(collect(&mut reader).await[0]["code"], "turn_not_found");
}

/// 断开后宽限到了还没人续上：这一轮被打断、收掉，再续上时不在了。
#[tokio::test]
async fn an_orphaned_turn_is_interrupted_after_the_grace() {
    let model = Model::start(true);
    let serve = Serve::start(&model, &["--grace", "1"], &[]);
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(&open("u", "t", json!({"new": UUID})))
        .await
        .unwrap();
    writer.send_text(&user_line("wait")).await.unwrap();
    assert_eq!(next(&mut reader).await["type"], "system");
    drop((reader, writer));
    // 宽限 1 秒；打断后这一轮很快结束、被拿掉。续上会重置宽限，所以只在足够久之后看一次。
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (mut reader, mut writer) = serve.connect(TOKEN).await.unwrap();
    writer
        .send_text(&json!({"type": "session_attach", "user": "u", "turn_id": "t"}).to_string())
        .await
        .unwrap();
    assert_eq!(collect(&mut reader).await[0]["code"], "turn_not_found");
    model.release();
}

/// PG 版（`scripts/rust-cli/pg-store.mjs` 的验收里跑）：两个用户的会话在同一个共用库里互相看不见；连接池上限 2。
#[tokio::test]
#[ignore = "requires an isolated PG (ZCODE_TEST_POSTGRES_URL)"]
async fn users_are_isolated_in_a_shared_pg() {
    // 修复：serve 走产品的连库（`connect_configured`），URL 不写 `sslmode` 时按 verify-full（pg-store.md §11），
    // 本机的测试 PG 没开 TLS，进程在打出 serve_ready 之前就退出了。测试库的 URL 不带参数（pg-store.mjs 校验），这里显式关 TLS。
    let url = format!(
        "{}?sslmode=disable",
        std::env::var(rust_cli_base::names::env::ZCODE_TEST_POSTGRES_URL).unwrap()
    );
    let model = Model::start(false);
    let serve = Serve::start(
        &model,
        &["--pg-url-env", "SERVE_PG", "--pg-pool", "2"],
        &[("SERVE_PG", &url)],
    );
    let suffix = format!(
        "{:x}",
        Instant::now().elapsed().as_nanos() ^ u128::from(std::process::id())
    );
    let (alice, bob) = (
        format!("serve-alice-{suffix}"),
        format!("serve-bob-{suffix}"),
    );
    let uuid = uuid_for(&suffix);
    let (first, second) = tokio::join!(
        turn(&serve, &alice, "t-1", json!({"new": uuid}), "from alice"),
        turn(&serve, &bob, "t-1", json!({"new": uuid}), "from bob"),
    );
    assert_eq!(result(&first)["subtype"], "success", "{first:?}");
    assert_eq!(result(&second)["subtype"], "success", "{second:?}");
    let resumed = turn(&serve, &alice, "t-2", json!({"resume": uuid}), "again").await;
    assert_eq!(result(&resumed)["subtype"], "success", "{resumed:?}");
    let history = serde_json::to_string(&model.turns().last().unwrap()["messages"]).unwrap();
    assert!(
        history.contains("from alice") && !history.contains("from bob"),
        "{history}"
    );
    let stranger = turn(
        &serve,
        &format!("serve-carol-{suffix}"),
        "t-1",
        json!({"resume": uuid}),
        "hi",
    )
    .await;
    assert_eq!(stranger[0]["code"], "session_not_found", "{stranger:?}");
}

/// 每次运行一个不同的 UUID（共用库里不撞上次的会话）。
fn uuid_for(seed: &str) -> String {
    let hex: String = format!("{seed:0>32}").chars().rev().take(32).collect();
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}
