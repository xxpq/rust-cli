//! 本机存储测试的夹具：临时存储根、写文件、本机 HTTP 服务（给 zip 与市场清单，不联网）。
#![allow(
    clippy::disallowed_methods,
    reason = "测试的本机 HTTP 服务直接用 tokio 的 TCP"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_paths::StoragePaths;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{Gate, Layout, LocalStore};

pub(crate) struct Fixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) gate: Gate,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            gate: Gate::new(),
        }
    }

    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    pub(crate) async fn store(&self) -> LocalStore {
        let home = self.dir.path().join("home");
        let paths = StoragePaths::resolve(&Default::default(), &home, &home);
        let layout = Layout::new(
            &paths.plugin_storage(None, &home),
            &paths.plugin_secrets(None, &home),
        );
        let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
        LocalStore::new(layout, egress, self.gate.clone())
    }

    pub(crate) async fn write(&self, relative: &str, text: impl AsRef<[u8]>) {
        rust_cli_fs::write_file(&self.path(relative), text.as_ref())
            .await
            .unwrap();
    }

    pub(crate) async fn json(&self, relative: &str, value: &Value) {
        self.write(relative, format!("{value:#}")).await;
    }
}

pub(crate) async fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&rust_cli_fs::read_optional(path).await.unwrap().unwrap()).unwrap()
}

/// 本机 HTTP 服务：每个请求回 `body`（状态码 `status`），返回地址。
pub(crate) async fn serve(status: u16, body: Vec<u8>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = [0_u8; 4096];
                let _ = socket.read(&mut request).await;
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    format!("http://{address}")
}
