//! 系统 git 走出口的代理（plugin-store.md §9.4）：真实的 git 子进程，代理是本机的一个监听口，记下 git 发来的第一行。
#![allow(clippy::disallowed_methods, reason = "测试的代理是本机的 TCP 监听口")]
use std::sync::{Arc, Mutex};

use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_net::{NetworkPolicy, NoProxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// 仓库地址：不该被解析、也不该直连（`.invalid` 不存在）；git 只能经代理拿它。
const REPO: &str = "http://plugins.example.invalid/kit.git";
/// 代理回给 git 的：拒绝，clone 失败（这个测试只看 git 有没有找代理）。
const REFUSED: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// 一个只记第一行的代理：返回地址与记下的行。
async fn proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = vec![0_u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
            record
                .lock_unpoisoned()
                .push(text.lines().next().unwrap_or_default().to_owned());
            let _ = socket.write_all(REFUSED).await;
        }
    });
    (address, seen)
}

#[tokio::test]
async fn system_git_goes_through_the_egress_proxy() {
    let (address, seen) = proxy().await;
    let egress = Egress::new(NetworkPolicy {
        proxy: Some(address),
        ..NetworkPolicy::default()
    })
    .await
    .unwrap();
    let work = tempfile::tempdir().unwrap();
    let repo = Repo {
        url: REPO,
        git_ref: None,
        sha: None,
        path: None,
        sparse: &[],
    };
    let result = repository(&egress, &repo, work.path(), &CancellationToken::new()).await;
    assert!(matches!(result, Err(StoreError::Fetch(_))), "{result:?}");
    let seen = seen.lock_unpoisoned().clone();
    assert!(
        seen.iter()
            .any(|line| line.contains("plugins.example.invalid")),
        "git did not ask the proxy: {seen:?}"
    );
}

#[test]
fn the_git_environment_carries_the_proxy_and_the_bypass_rules() {
    let policy = NetworkPolicy {
        proxy: Some("http://proxy.example.com:8080".to_owned()),
        no_proxy: NoProxy::Node("localhost,.example.org".to_owned()),
        ..NetworkPolicy::default()
    };
    let env = git_proxy_env(&policy);
    let value = |name: &str| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    };
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        assert_eq!(value(name), Some("http://proxy.example.com:8080"), "{name}");
    }
    assert_eq!(value("NO_PROXY"), Some("localhost,.example.org"));
    assert_eq!(value("no_proxy"), Some("localhost,.example.org"));
    // 没有代理：什么都不设（入口算代理时已经读过标准变量，进程里的那些照样继承）
    assert!(git_proxy_env(&NetworkPolicy::default()).is_empty());
}
