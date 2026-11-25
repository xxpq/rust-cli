//! 适配器（ext.md §3.11）：通知按到达顺序交给连接任务、不交给 rmcp（G11）。

use futures_util::FutureExt;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::mcp::{McpConnect, McpTransportConfig};
use serde_json::json;

use super::super::connection::REVISIONS;
use super::super::notices;
use super::*;
use crate::test_support::{FakeMcp, server, workspace};

/// G11：通知在读的一侧按到达顺序交给连接任务，rmcp 只拿到之后的消息。rmcp 把每条通知放进单独的任务处理，与之后的消息、
/// 与连接结束都没有先后；适配器把通知交给 rmcp 时，`receive` 先交出的是通知本身，这里失败。
#[tokio::test]
async fn notifications_go_to_the_connection_task_in_order() {
    let fake = Arc::new(FakeMcp::default().with("s", server(json!({}), None, |_, _| None)));
    let transport: Arc<dyn McpTransport> = fake.clone();
    let request = McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::stdio("node", Vec::new()),
    };
    let connection = transport.connect(&workspace(), request).await.unwrap();
    let scope = TaskScope::root("test", |_| {});
    let (sender, mut receiver) = notices::channel();
    let mut link = Link::new(
        transport,
        connection,
        scope.spawner(),
        Arc::default(),
        sender,
        (&REVISIONS, None),
    );
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
    );
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "x"}}),
    );
    fake.push("s", json!({"jsonrpc": "2.0", "id": "p1", "method": "ping"}));
    let first = link.receive().await;
    assert!(
        matches!(&first, Some(JsonRpcMessage::Request(request)) if matches!(request.request, ServerRequest::PingRequest(_))),
        "{first:?}"
    );
    assert_eq!(
        receiver.recv().now_or_never(),
        Some(Some(Notice::ListChanged("tools".to_owned())))
    );
    fake.drop_server("s", false);
    assert!(link.receive().await.is_none());
    assert_eq!(
        receiver.recv().now_or_never(),
        Some(Some(Notice::Closed {
            reason: Some("gone".to_owned()),
            session_expired: false
        }))
    );
}
