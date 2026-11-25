//! 本地的假 npm 注册表（docs/specs/rust-cli/update.md §9、§12）：`zcode update` 先查最新版本，真实进程测试经
//! `_ZCODE_ASSUME_NPM_REGISTRY` 把它交给 debug 构建，不联网。
use std::io::{BufRead as _, BufReader, Write as _};

/// 假注册表上入口包与平台包的最新版本；debug 构建自己是 0.1.0。
pub const LATEST: &str = "0.1.55";

/// 本地的假 npm 注册表（进程里起一次）：任何包的 `dist-tags.latest` 都是 [`LATEST`]，这个版本有 tarball 与 integrity。
pub fn url() -> &'static str {
    static URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    URL.get_or_init(|| {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("");
                let name = path.trim_start_matches('/').replace("%2f", "/");
                let body = serde_json::json!({
                    "name": name,
                    "dist-tags": {"latest": LATEST},
                    "versions": {LATEST: {"name": name, "version": LATEST, "dist": {
                        "tarball": format!("https://registry.example.com/{name}/-/{LATEST}.tgz"),
                        "integrity": format!("sha512-{LATEST}"),
                    }}},
                })
                .to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        url
    })
}
