//! 远程 MCP 服务器的 OAuth（docs/specs/rust-cli/ext.md §3.12）：真实 `zcode -p` 连一个要 Bearer 的本地 HTTP MCP 服务器。
//! 凭据文件里放 Node 写的形状的令牌（明文值，受管密钥，不碰系统凭据库）：快过期时先在授权服务器刷新，带新令牌调用工具，
//! 新令牌按 Node 的键写回；没有令牌的服务器在 `system/init` 里是 `needs-auth`，不打开浏览器。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod sse;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use rust_cli_base::models::GLM_5_3;
use serde_json::{Value, json};

/// Node 的键的前缀：`createCredentialKeyPrefix("docs", <url>, {type: "authorization_code"})` 的算法（URL 每次不同，测试里现算，
/// 与 Node 逐字相同的对拍在 platform 的 golden）。
fn prefix(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("docs\n{url}\n\n\n").as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("mcp:oauth:{}", &hex[..24])
}

/// 一个请求：方法、路径、`Authorization`、正文。
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

/// 受保护资源（`/mcp`，只认 `Bearer at-2`）与它的授权服务器（`/as`）。
fn serve() -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let url = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let mut parts = first.split_whitespace();
            let (method, path) = (
                parts.next().unwrap_or_default().to_owned(),
                parts.next().unwrap_or_default().to_owned(),
            );
            let (mut length, mut authorization) = (0, None);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    match name.trim().to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap_or(0),
                        "authorization" => authorization = Some(value.trim().to_owned()),
                        _ => {}
                    }
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).into_owned();
            log.lock().unwrap().push(Seen {
                method: method.clone(),
                path: path.clone(),
                authorization: authorization.clone(),
                body: body.clone(),
            });
            let (status, extra, reply) =
                answer(&url, &method, &path, authorization.as_deref(), &body);
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (base, seen)
}

fn answer(
    base: &str,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    body: &str,
) -> (&'static str, String, String) {
    const JSON: &str = "content-type: application/json\r\n";
    match (method, path) {
        ("GET", "/.well-known/oauth-protected-resource/mcp") => (
            "200 OK",
            JSON.to_owned(),
            json!({"resource": format!("{base}/mcp"), "authorization_servers": [format!("{base}/as")]}).to_string(),
        ),
        ("GET", "/.well-known/oauth-authorization-server/as") => (
            "200 OK",
            JSON.to_owned(),
            json!({"issuer": format!("{base}/as"), "authorization_endpoint": format!("{base}/as/authorize"),
                   "token_endpoint": format!("{base}/as/token"), "response_types_supported": ["code"]})
            .to_string(),
        ),
        ("POST", "/as/token") if body.contains("refresh_token=rt-1") => (
            "200 OK",
            JSON.to_owned(),
            json!({"access_token": "at-2", "token_type": "Bearer", "expires_in": 3600}).to_string(),
        ),
        ("POST", "/mcp") if authorization == Some("Bearer at-2") => {
            let message: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let Some(id) = message.get("id") else {
                return ("202 Accepted", String::new(), String::new());
            };
            let result = match message["method"].as_str() {
                Some("initialize") => json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                                             "serverInfo": {"name": "docs", "version": "1"}}),
                Some("tools/list") => json!({"tools": [{"name": "ping", "inputSchema": {"type": "object"}}]}),
                Some("tools/call") => json!({"content": [{"type": "text", "text": "pong"}]}),
                _ => json!({}),
            };
            ("200 OK", JSON.to_owned(), json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string())
        }
        ("GET", "/mcp") => ("405 Method Not Allowed", String::new(), String::new()),
        _ => (
            "401 Unauthorized",
            format!("www-authenticate: Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"\r\n"),
            String::new(),
        ),
    }
}

/// 凭据文件里 Node 写的 canonical 与两份镜像（明文值，Node 也读）。`expires_at` 是 Unix 毫秒。
fn seed(home: &Path, url: &str, expires_at: u64) {
    let prefix = prefix(url);
    let client = json!({"client_id": "dyn-1"});
    let tokens = json!({"access_token": "at-1", "token_type": "Bearer", "expires_in": 3600, "refresh_token": "rt-1"});
    let canonical = json!({
        "client_information": client, "expires_at": expires_at, "generation": "0123456789abcdef0123456789abcdef",
        "obtained_at": expires_at - 3_600_000, "published_by": "tx", "tokens": tokens, "version": 2
    });
    let record = json!({
        format!("{prefix}:authorization_credentials"): canonical.to_string(),
        format!("{prefix}:client_information"): client.to_string(),
        format!("{prefix}:tokens"): tokens.to_string(),
    });
    let dir = home.join(".zcode/v2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("credentials.json"), record.to_string()).unwrap();
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

/// `zcode -p`：MCP 服务器经 `--mcp-config`（`mcpServers` 的写法）给；工具搜索关掉，MCP 工具直接在工具表里。
fn run(home: &Path, work: &Path, mcp_url: &str, model: &str, stream: bool) -> std::process::Output {
    let config = json!({"mcpServers": {"docs": {"type": "http", "url": mcp_url}}});
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args([
            "-p",
            "hi",
            "--allowedTools",
            "mcp__docs",
            "--strict-mcp-config",
            "--mcp-config",
        ])
        .arg(config.to_string());
    if stream {
        command.args(["--output-format", "stream-json", "--verbose"]);
    }
    command
        .current_dir(work)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
        // 受管密钥：凭据文件的值不经系统凭据库（platform.md §13.2）
        .env("ZCODE_CREDENTIAL_SECRET", "test-secret")
        .env("ZCODE_MODEL_BASE_URL", model)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3)
        .env("ENABLE_TOOL_SEARCH", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    (dir, home, work)
}

#[test]
fn node_written_tokens_are_refreshed_before_they_expire_and_carry_the_tool_call() {
    let (_dir, home, work) = dirs();
    let (base, seen) = serve();
    let mcp = format!("{base}/mcp");
    // 60 s 后过期：在 300 s 的余量里，第一个请求之前先刷新
    seed(&home, &mcp, now_ms() + 60_000);
    let model = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_1", "mcp__docs__ping", "{}")]),
        sse::text_reply("done"),
    ]);
    let output = run(&home, &work, &mcp, &model.url, false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let seen = seen.lock().unwrap().clone();
    let refresh = seen
        .iter()
        .find(|s| s.path == "/as/token")
        .expect("a refresh request");
    assert!(
        refresh.body.contains("grant_type=refresh_token"),
        "{refresh:?}"
    );
    assert!(refresh.body.contains("client_id=dyn-1"), "{refresh:?}");
    assert!(
        refresh
            .body
            .contains(&format!("resource={}", urlencode(&mcp))),
        "{refresh:?}"
    );
    // MCP 的每个请求都带新令牌，没有一个被拒
    let posts: Vec<&Seen> = seen
        .iter()
        .filter(|s| s.path == "/mcp" && s.method == "POST")
        .collect();
    assert!(!posts.is_empty());
    assert!(
        posts
            .iter()
            .all(|s| s.authorization.as_deref() == Some("Bearer at-2")),
        "{posts:?}"
    );
    // 工具结果到了模型
    let second = model.main_requests().remove(1).to_string();
    assert!(second.contains("pong"), "{second}");
    // 新令牌按 Node 的键写回（值按文件的版本加密）
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".zcode/v2/credentials.json")).unwrap(),
    )
    .unwrap();
    let canonical = record[format!("{}:authorization_credentials", prefix(&mcp))]
        .as_str()
        .unwrap();
    assert!(canonical.starts_with("enc:v1:"), "{canonical}");
}

#[test]
fn a_server_without_tokens_needs_auth_and_no_browser_is_opened() {
    let (_dir, home, work) = dirs();
    let (base, seen) = serve();
    let mcp = format!("{base}/mcp");
    let model = sse::serve(vec![sse::text_reply("done")]);
    let output = run(&home, &work, &mcp, &model.url, true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let init: Value = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["type"] == "system" && event["subtype"] == "init")
        .expect("system/init");
    assert_eq!(
        init["mcp_servers"],
        json!([{"name": "docs", "status": "needs-auth"}]),
        "{init}"
    );
    // 被动一侧不发现、不注册、不开监听：只有 MCP 的请求
    let seen = seen.lock().unwrap().clone();
    assert!(seen.iter().all(|s| s.path == "/mcp"), "{seen:?}");
}

fn urlencode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
