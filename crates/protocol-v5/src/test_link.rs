//! 进程内测试的链路：真实的 actor 与 Content-Length 分帧，两头是内存管道。`server_tests` 与 `sessions_tests` 共用。
use std::time::Duration;

use rust_cli_base::TaskScope;
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_core::framing::{Dialect, Framing};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, duplex};

use crate::generated::contract::CONTRACT_DIGEST;
use crate::server::{Handle, Settings, start};

/// 管道的缓冲（字节）：测试的消息都远小于它。
const PIPE_BYTES: usize = 1 << 20;
/// 等一条消息的上限：只决定“等多久算失败”，不参与判定。
const READ_BUDGET: Duration = Duration::from_secs(5);

pub(crate) struct Harness {
    to: DuplexStream,
    from: BufReader<DuplexStream>,
    pub(crate) handle: Handle,
    _scope: TaskScope,
}

impl Harness {
    pub(crate) fn new() -> Self {
        Self::with(Settings {
            version: "test".to_owned(),
            plugins: None,
            mcp_oauth: false,
        })
    }

    /// 入口装了 MCP 的授权（protocol-v5.md §74）。
    pub(crate) fn with_mcp_oauth() -> Self {
        Self::with(Settings {
            version: "test".to_owned(),
            plugins: None,
            mcp_oauth: true,
        })
    }

    /// 入口交了插件管理的端口（protocol-v5.md §72）。
    pub(crate) fn with_plugins(
        admin: std::sync::Arc<dyn crate::plugin_admin::PluginAdmin>,
    ) -> Self {
        Self::with(Settings {
            version: "test".to_owned(),
            plugins: Some(admin),
            mcp_oauth: false,
        })
    }

    fn with(settings: Settings) -> Self {
        let scope = TaskScope::root("test", |_| {});
        let options = Options {
            framing: Framing::ContentLength,
            dialect: Dialect::JsonRpc2,
            ..Options::default()
        };
        let (to, server_in) = duplex(PIPE_BYTES);
        let (server_out, from) = duplex(PIPE_BYTES);
        let (peer, _writer) = connection::start(server_out, &scope.spawner(), options);
        let (handle, intake, _task) = start(peer.clone(), settings, &scope.spawner());
        drop(scope.spawner().spawn(
            "read",
            connection::serve_in_order(BufReader::new(server_in), peer, intake, options),
        ));
        Self {
            to,
            from: BufReader::new(from),
            handle,
            _scope: scope,
        }
    }

    pub(crate) async fn send(&mut self, message: Value) {
        let body = message.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.to.write_all(frame.as_bytes()).await.unwrap();
    }

    pub(crate) async fn next(&mut self) -> Value {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let byte = tokio::time::timeout(READ_BUDGET, self.from.read_u8())
                .await
                .unwrap()
                .unwrap();
            head.push(byte);
        }
        let head = String::from_utf8(head).unwrap();
        let length: usize = head
            .trim()
            .trim_start_matches("Content-Length: ")
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        self.from.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// 关掉送给 actor 的输入（对端走了）：读循环读到结束，链路断开。
    pub(crate) async fn close_input(&mut self) {
        self.to.shutdown().await.unwrap();
    }

    /// 读到对这个 id 的回复为止（中间的帧交给 `frames`）。
    pub(crate) async fn reply(&mut self, id: u64, frames: &mut Vec<Value>) -> Value {
        loop {
            let message = self.next().await;
            if message["id"] == json!(id) {
                return message;
            }
            frames.push(message);
        }
    }
}

/// 严格链路的握手：带本端的契约指纹（03 §3.8）。
pub(crate) fn initialize() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
           "params": {"clientId": "t", "caps": {"core": [1], "agent": [1]}, "contract": CONTRACT_DIGEST}})
}

/// 宽松链路的握手：不带指纹。
pub(crate) fn initialize_tolerant() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
           "params": {"clientId": "t", "caps": {"core": [1], "agent": [1]}}})
}

/// 没有会话的内核（只测 agent 与会话列表的进程内测试用）：工作区端口的 `open` 给出它。会话的行为在真进程的一致性场景里测
/// （protocol-v5.md §15.11）。
struct NoSessions(std::sync::Arc<rust_cli_kernel_api::testing::MemoryStore>);

#[async_trait::async_trait]
impl rust_cli_kernel_api::session_api::Kernel for NoSessions {
    async fn create_session(
        &self,
        _request: rust_cli_kernel_api::session_api::CreateSession,
    ) -> Result<rust_cli_kernel_api::ids::SessionId, rust_cli_kernel_api::session_api::KernelError>
    {
        Err(rust_cli_kernel_api::session_api::KernelError::Unsupported {
            what: "sessions".to_owned(),
        })
    }

    async fn open_session(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
    ) -> Result<
        std::sync::Arc<dyn rust_cli_kernel_api::session_api::SessionHandle>,
        rust_cli_kernel_api::session_api::KernelError,
    > {
        Err(rust_cli_kernel_api::session_api::KernelError::NotFound {
            session: session.clone(),
        })
    }

    async fn list_sessions(
        &self,
        _filter: rust_cli_kernel_api::store::SessionFilter,
    ) -> Result<
        Vec<rust_cli_kernel_api::store::SessionSummary>,
        rust_cli_kernel_api::session_api::KernelError,
    > {
        Ok(Vec::new())
    }

    fn queries(&self) -> std::sync::Arc<dyn rust_cli_kernel_api::store::StoreQueries> {
        self.0.clone()
    }
}

/// 工作区端口 `open` 的结果（测试用：没有会话的内核）。
pub(crate) fn kernel() -> crate::server::WorkspaceKernel {
    crate::server::WorkspaceKernel {
        kernel: std::sync::Arc::new(NoSessions(std::sync::Arc::default())),
        selection: rust_cli_kernel_api::testing::selection(),
        mode: rust_cli_kernel_api::permission::PermissionMode::Default,
    }
}
