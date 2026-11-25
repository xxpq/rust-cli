//! JSON-RPC 客户端：请求配对、服务器请求的回答、通知、关闭（ext.md §3.3）；rmcp 做不到、在外面补的几处（§3.11.2 的编号）
//! 各有一个用例，去掉补法（用 rmcp 原样的做法）时失败。

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::BoxStream;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransportConfig, McpTransportEvent,
};
use serde_json::json;
use tokio::sync::watch;
use tokio::time::Instant;

use super::super::connection::REVISIONS;
use super::*;
use crate::test_support::{FakeMcp, server, workspace};

/// 在 `transport` 上建连接、握手（服务器 `s`）。
async fn connected_on(
    transport: Arc<dyn McpTransport>,
    scope: &TaskScope,
) -> (Arc<Client>, NoticeReceiver) {
    let request = McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::http(
            "https://mcp.example.com".to_owned(),
            Default::default(),
        ),
    };
    let id = transport.connect(&workspace(), request).await.unwrap();
    let opened = Client::connect(
        transport,
        id,
        &scope.spawner(),
        vec!["/Users/dev/project".to_owned()],
        &REVISIONS,
        ("1", None),
    )
    .await
    .unwrap();
    (opened.client, opened.notices)
}

async fn connected(fake: &Arc<FakeMcp>, scope: &TaskScope) -> (Arc<Client>, NoticeReceiver) {
    connected_on(fake.clone(), scope).await
}

/// 请求 `method` 到达传输时它的 ID（rmcp 分的；握手的 `initialize` 用掉 0）。
async fn id_of(fake: &FakeMcp, method: &str) -> Value {
    fake.arrival(|message| message["method"] == method).await["id"].clone()
}

fn is_cancel(message: &Value) -> bool {
    message["method"] == "notifications/cancelled"
}

#[tokio::test]
async fn requests_pair_with_responses_and_errors() {
    let fake = Arc::new(FakeMcp::default().with(
        "s",
        server(json!({}), None, |method, params| match method {
            "echo" => Some(Ok(params.clone())),
            "boom" => Some(Err((-32000, "bad".to_owned()))),
            _ => None,
        }),
    ));
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let long = Duration::from_secs(5);
    assert_eq!(
        client.request("echo", json!({"a": 1}), long, &never).await,
        Ok(json!({"a": 1}))
    );
    assert_eq!(
        client.request("boom", json!({}), long, &never).await,
        Err(RpcError::Server {
            code: -32000,
            message: "bad".to_owned()
        })
    );
    // 不回答的请求：取消时发 notifications/cancelled（在后台发，结果不等它，ext.md §3.10.9）。
    let cancel = CancellationToken::new();
    let request = client.request("hang", json!({}), long, &cancel);
    let interrupt = async {
        id_of(&fake, "hang").await;
        cancel.cancel();
    };
    let (outcome, ()) = tokio::join!(request, interrupt);
    assert_eq!(outcome, Err(RpcError::Cancelled));
    let cancelled = fake.arrival(is_cancel).await;
    assert_eq!(cancelled["params"]["requestId"], id_of(&fake, "hang").await);
}

/// 缺陷（v5r-mcp3 线，ext.md §3.10.6 的“续不上时”）：传输说回复不会来了（`ReplyLost`）时，请求以前等到它自己的期限
/// （工具调用缺省约 27.8 小时）。现在立即以传输给的原因失败，并告诉服务器不用再做（同超时）；连接照常。rmcp 的传输接口
/// 没有这种事件，适配器合成错误回复、包装层从表里还原原因（§3.11 的 G5）：适配器不合成时请求等到期限，这里失败。
#[tokio::test]
async fn a_lost_reply_fails_the_request_at_once_and_cancels_it() {
    let reason = "MCP response stream ended before the reply, with no event ID to resume from";
    let fake = Arc::new(
        FakeMcp::default()
            .with(
                "s",
                server(json!({}), None, |method, params| {
                    (method == "echo").then(|| Ok(params.clone()))
                }),
            )
            .losing("hang", reason),
    );
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let deadline = Duration::from_secs(100_000);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client.request("hang", json!({}), deadline, &never),
    )
    .await
    .expect("the request ends without waiting for its deadline");
    assert_eq!(outcome, Err(RpcError::ReplyLost(reason.to_owned())));
    assert_eq!(
        fake.arrival(is_cancel).await["params"],
        json!({"requestId": id_of(&fake, "hang").await, "reason": "Response stream lost"})
    );
    assert_eq!(
        client
            .request("echo", json!({"a": 1}), deadline, &never)
            .await,
        Ok(json!({"a": 1}))
    );
}

/// 服务器自己的错误回复不会被当成“回复不会来了”：原因表只由传输的 `ReplyLost` 写（G5）。
#[tokio::test]
async fn a_server_error_is_not_taken_for_a_lost_reply() {
    let reason = "MCP response stream ended before the reply, with no event ID to resume from";
    let fake = Arc::new(
        FakeMcp::default()
            .with(
                "s",
                server(json!({}), None, |method, _| {
                    (method == "boom").then(|| Err((-32603, reason.to_owned())))
                }),
            )
            .losing("hang", reason),
    );
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let long = Duration::from_secs(5);
    assert_eq!(
        client.request("boom", json!({}), long, &never).await,
        Err(RpcError::Server {
            code: -32603,
            message: reason.to_owned()
        })
    );
    assert_eq!(
        client.request("hang", json!({}), long, &never).await,
        Err(RpcError::ReplyLost(reason.to_owned()))
    );
}

/// 出口的连接期限（net 缺省 30 s）：网络不通时取消的发送最多等这么久，请求的结果不能等它。
const EGRESS_DEADLINE: Duration = Duration::from_secs(30);
const LOST: &str = "MCP response stream ended before the reply, with no event ID to resume from";

/// `hang` 不回答，`lost` 的回复不会来（`ReplyLost`），取消的发送记下之后一直不结束（网络不通）。
fn stalling_fake() -> Arc<FakeMcp> {
    Arc::new(
        FakeMcp::default()
            .with("s", server(json!({}), None, |_, _| None))
            .losing("lost", LOST)
            .stalling("notifications/cancelled"),
    )
}

/// 缺陷（v5r-mcp4 线，ext.md §3.10.9）：请求到期时，结果以前等 `notifications/cancelled` 发完才交回；网络不通时这次
/// 发送要等出口的连接期限，工具的错误结果跟着晚 30 s。现在结果立即交回，取消在后台照发。rmcp 自带的期限正是旧的做法
/// （§3.11 的 G3）：用它时这里失败。
#[tokio::test(start_paused = true)]
async fn a_timed_out_request_does_not_wait_for_its_cancel() {
    let fake = stalling_fake();
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let outcome = tokio::time::timeout(
        EGRESS_DEADLINE,
        client.request("hang", json!({}), Duration::from_millis(100), &never),
    )
    .await
    .expect("the result does not wait for the cancel to be sent");
    assert_eq!(outcome, Err(RpcError::TimedOut));
    let cancelled = fake.arrival(is_cancel).await;
    assert_eq!(
        cancelled["params"],
        json!({"requestId": id_of(&fake, "hang").await, "reason": "Request timed out"})
    );
}

/// 同上，回复不会来了（`ReplyLost`）的路径：v5r-mcp3 线照超时做，同样先等取消发完。
#[tokio::test(start_paused = true)]
async fn a_request_whose_reply_is_lost_does_not_wait_for_its_cancel() {
    let fake = stalling_fake();
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let outcome = tokio::time::timeout(
        EGRESS_DEADLINE,
        client.request("lost", json!({}), Duration::from_secs(100_000), &never),
    )
    .await
    .expect("the result does not wait for the cancel to be sent");
    assert_eq!(outcome, Err(RpcError::ReplyLost(LOST.to_owned())));
    let cancelled = fake.arrival(is_cancel).await;
    assert_eq!(
        cancelled["params"],
        json!({"requestId": id_of(&fake, "lost").await, "reason": "Response stream lost"})
    );
}

/// 测试自己的时间上限（虚拟时间）：旧代码挂在这里，用它判失败，测试不会真的无界地等。
const TEST_LIMIT: Duration = Duration::from_secs(60);

/// `slow` 的发送记下之后一直不结束：服务器收下请求、不回响应头（streamable HTTP 与 SSE 的 `send` 等到响应头才返回）。
fn unanswered_send_fake() -> Arc<FakeMcp> {
    Arc::new(
        FakeMcp::default()
            .with("s", server(json!({}), None, |_, _| None))
            .stalling("slow"),
    )
}

/// 缺陷（v5r-mcp5 线，ext.md §3.10.10）：期限以前从 `send` 返回之后才算起，服务器收下请求、不回响应头时，请求越过
/// 期限一直等。现在期限覆盖发送：到期按超时结束，照发取消（服务器可能已经收下了）。
#[tokio::test(start_paused = true)]
async fn a_request_whose_send_stalls_ends_at_its_deadline() {
    let fake = unanswered_send_fake();
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let outcome = tokio::time::timeout(
        TEST_LIMIT,
        client.request("slow", json!({}), Duration::from_millis(100), &never),
    )
    .await
    .expect("the deadline covers the send");
    assert_eq!(outcome, Err(RpcError::TimedOut));
    assert_eq!(
        fake.arrival(is_cancel).await["params"],
        json!({"requestId": id_of(&fake, "slow").await, "reason": "Request timed out"})
    );
}

/// 同上，用户中止落在发送中途：以前要等 `send` 返回才看得到中止。
#[tokio::test(start_paused = true)]
async fn a_request_whose_send_stalls_ends_when_interrupted() {
    let fake = unanswered_send_fake();
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let cancel = CancellationToken::new();
    let request = client.request("slow", json!({}), Duration::from_secs(100_000), &cancel);
    // 请求已经交给传输、停在发送上，再中止。
    let interrupt = async {
        id_of(&fake, "slow").await;
        cancel.cancel();
    };
    let (outcome, ()) =
        tokio::time::timeout(TEST_LIMIT, async { tokio::join!(request, interrupt) })
            .await
            .expect("an interrupt reaches a request in its send");
    assert_eq!(outcome, Err(RpcError::Cancelled));
    assert_eq!(
        fake.arrival(is_cancel).await["params"],
        json!({"requestId": id_of(&fake, "slow").await, "reason": "User interrupted"})
    );
}

/// 请求 `method` 发出（传输收到）之后，服务器再推来 `messages(它的 ID)`。
async fn after_sent(fake: &FakeMcp, method: &str, messages: impl FnOnce(Value) -> Vec<Value>) {
    let id = id_of(fake, method).await;
    for message in messages(id) {
        fake.push("s", message);
    }
}

/// 缺陷（v5r-mcp6 线，ext.md §3.10.11）：回复先于 `send` 返回时（SSE 传输的回复走事件流，POST 的响应可以更晚；远端
/// 机器上 `mcp.send` 的回执与事件流没有先后），请求以前还等 `send` 返回，POST 一直不回时到期按超时结束、回复丢掉。
/// 现在回复到了就结束请求，不发取消（没完成的发送照样跑到结束，§3.11 的 G4）。
#[tokio::test(start_paused = true)]
async fn a_reply_that_comes_before_its_send_returns_ends_the_request() {
    let fake = unanswered_send_fake();
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let request = client.request("slow", json!({}), Duration::from_secs(100_000), &never);
    let reply = |id| vec![json!({"jsonrpc": "2.0", "id": id, "result": {"early": true}})];
    let (outcome, ()) = tokio::time::timeout(TEST_LIMIT, async {
        tokio::join!(request, after_sent(&fake, "slow", reply))
    })
    .await
    .expect("the reply ends the request while its send is still out");
    assert_eq!(outcome, Ok(json!({"early": true})));
    assert!(
        !fake
            .received()
            .iter()
            .any(|(_, message)| is_cancel(message))
    );
}

/// 同上，发送中途连接关了：等待方以 `connection closed` 失败，请求立即结束。以前等 `send` 返回才拿到这个错误，
/// `send` 一直不返回时到期按超时结束。
#[tokio::test(start_paused = true)]
async fn a_request_whose_send_stalls_ends_when_the_connection_closes() {
    let fake = unanswered_send_fake();
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let request = client.request("slow", json!({}), Duration::from_secs(100_000), &never);
    let close = async {
        id_of(&fake, "slow").await;
        fake.drop_server("s", false);
    };
    let (outcome, ()) = tokio::time::timeout(TEST_LIMIT, async { tokio::join!(request, close) })
        .await
        .expect("closing the connection ends a request in its send");
    assert_eq!(
        outcome,
        Err(RpcError::Closed("connection closed".to_owned()))
    );
}

/// 缺陷（v5r-mcp6 线，ext.md §3.10.11）：读者以前在读的循环里等对服务器请求的回答发完。服务器收下回答的 POST、不回
/// 响应头时读者停住，之后到的回复交不出，请求等到它自己的期限（MCP-5 实测：回复 300 ms 就到，请求 1.55 s 报超时）。
/// 现在回答在后台发，读者只读：回复照常交回，一个卡住的回答也不挡别的回答。
#[tokio::test(start_paused = true)]
async fn a_stalled_answer_to_a_server_request_does_not_stop_the_reader() {
    let fake = Arc::new(
        FakeMcp::default()
            .with("s", server(json!({}), None, |_, _| None))
            .stalling_answers(),
    );
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let request = client.request("work", json!({}), Duration::from_secs(100_000), &never);
    // 同一个 SSE 流上，服务器在回复之前先发两个请求。
    let server = after_sent(&fake, "work", |id| {
        vec![
            json!({"jsonrpc": "2.0", "id": "p1", "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": "p2", "method": "roots/list"}),
            json!({"jsonrpc": "2.0", "id": id, "result": {"done": true}}),
        ]
    });
    let (outcome, ()) = tokio::time::timeout(TEST_LIMIT, async { tokio::join!(request, server) })
        .await
        .expect("the reply reaches the request while an answer is stalled");
    assert_eq!(outcome, Ok(json!({"done": true})));
    let answered = |id: &'static str| fake.arrival(move |message| message["id"] == id);
    let (ping, roots) = tokio::time::timeout(TEST_LIMIT, async {
        tokio::join!(answered("p1"), answered("p2"))
    })
    .await
    .expect("both answers reach the transport");
    assert_eq!(ping, json!({"jsonrpc": "2.0", "id": "p1", "result": {}}));
    assert_eq!(
        roots,
        json!({"jsonrpc": "2.0", "id": "p2", "result": {"roots": [{"uri": "file:///Users/dev/project"}]}})
    );
}

/// 替身传输加两样：`close` 要花 `takes`（stdio 的关闭要关 stdin、TERM 后 KILL 进程树），记下第一次被调用的时刻。
struct SlowClose {
    fake: Arc<FakeMcp>,
    takes: Duration,
    called: watch::Sender<Option<Instant>>,
}

impl SlowClose {
    fn new(fake: Arc<FakeMcp>, takes: Duration) -> Arc<Self> {
        Arc::new(Self {
            fake,
            takes,
            called: watch::channel(None).0,
        })
    }

    /// 等到 `close` 被调用，返回那一刻。
    async fn close_called(&self) -> Instant {
        let mut called = self.called.subscribe();
        let at = called.wait_for(Option::is_some).await.unwrap();
        at.unwrap()
    }
}

#[async_trait::async_trait]
impl McpTransport for SlowClose {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        self.fake.connect(workspace, request).await
    }

    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError> {
        self.fake.send(connection, message).await
    }

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        self.fake.events(connection)
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        self.called.send_if_modified(|called| {
            let first = called.is_none();
            called.get_or_insert_with(Instant::now);
            first
        });
        tokio::time::sleep(self.takes).await;
        self.fake.close(connection).await
    }
}

/// 关传输要花的时间（虚拟时间）。
const CLOSE_TAKES: Duration = Duration::from_secs(10);

/// 等 `client` 上一个不回答的请求失败，返回失败的时刻与结果。
fn pending(client: &Arc<Client>) -> tokio::task::JoinHandle<(Instant, Result<Value, RpcError>)> {
    let client = client.clone();
    tokio::spawn(async move {
        let outcome = client
            .request(
                "hang",
                json!({}),
                Duration::from_secs(100_000),
                &CancellationToken::new(),
            )
            .await;
        (Instant::now(), outcome)
    })
}

/// 连接关了时等待中的请求先失败，不等传输关完（ext.md §3.10.7、§3.11 的 G7）。rmcp 的等待表在它的循环结束时才释放，
/// 循环要先等 `close` 返回：适配器的 `close` 若等传输关完，请求在 `CLOSE_TAKES` 之后才失败，这里失败。
#[tokio::test(start_paused = true)]
async fn waiters_fail_before_the_transport_has_closed() {
    for explicit in [false, true] {
        let fake = Arc::new(FakeMcp::default().with("s", server(json!({}), None, |_, _| None)));
        let transport = SlowClose::new(fake.clone(), CLOSE_TAKES);
        let scope = TaskScope::root("test", |_| {});
        let (client, _notices) = connected_on(transport.clone(), &scope).await;
        let waiting = pending(&client);
        id_of(&fake, "hang").await;
        let closing = Instant::now();
        if explicit {
            // `/mcp reconnect`、停用、退出：显式关闭照样等传输关完（stdio 的进程树结束之后才返回）。
            client.close().await;
            assert!(
                Instant::now() - closing >= CLOSE_TAKES,
                "explicit close waits for the transport"
            );
        } else {
            fake.drop_server("s", false);
        }
        let (failed, outcome) = waiting.await.unwrap();
        assert_eq!(
            outcome,
            Err(RpcError::Closed("connection closed".to_owned()))
        );
        assert!(failed - closing < CLOSE_TAKES, "explicit: {explicit}");
    }
}

/// 卡住的回答不挡住关闭（ext.md §3.10.11、§3.11 的 G6）。rmcp 的循环结束时先排空在发的回答：服务器断开时最多等 5 s，
/// 最后的句柄放下时最多等 2 s。适配器若让 rmcp 等回答发完，等待中的请求晚 5 s 失败、传输晚 2 s 关闭，这里失败。
#[tokio::test(start_paused = true)]
async fn a_stalled_answer_does_not_hold_up_the_close() {
    let ping = json!({"jsonrpc": "2.0", "id": "p1", "method": "ping"});
    for released in [false, true] {
        let fake = Arc::new(
            FakeMcp::default()
                .with("s", server(json!({}), None, |_, _| None))
                .stalling_answers(),
        );
        let transport = SlowClose::new(fake.clone(), Duration::ZERO);
        let scope = TaskScope::root("test", |_| {});
        let (client, _notices) = connected_on(transport.clone(), &scope).await;
        let waiting = pending(&client);
        id_of(&fake, "hang").await;
        fake.push("s", ping.clone());
        fake.arrival(|message| message["id"] == "p1").await;
        let closing = Instant::now();
        if released {
            waiting.abort();
            drop(client);
            let called = transport.close_called().await;
            assert_eq!(called, closing, "the transport closes at once");
        } else {
            fake.drop_server("s", false);
            let (failed, outcome) = waiting.await.unwrap();
            assert_eq!(
                outcome,
                Err(RpcError::Closed("connection closed".to_owned()))
            );
            assert_eq!(failed, closing, "the waiter fails at once");
        }
    }
}

/// rmcp 给每个请求加的 `_meta.progressToken` 不发给服务器，`_meta` 里本工程的键留着（§3.11 的 G9；以前不带）。去掉删除时
/// 这里失败。
#[tokio::test]
async fn requests_carry_no_progress_token() {
    let fake = Arc::new(FakeMcp::default().with(
        "s",
        server(json!({}), None, |_, params| Some(Ok(params.clone()))),
    ));
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let long = Duration::from_secs(5);
    let call = json!({"name": "read", "arguments": {}, "_meta": {"zcode/toolUseId": "toolu_1"}});
    client
        .request("tools/list", json!({}), long, &never)
        .await
        .unwrap();
    client
        .request("tools/call", call.clone(), long, &never)
        .await
        .unwrap();
    let params = |method: &str| {
        fake.received()
            .into_iter()
            .find(|(_, message)| message["method"] == method)
            .map(|(_, message)| message["params"].clone())
    };
    assert_eq!(params("tools/list"), Some(json!({})));
    assert_eq!(params("tools/call"), Some(call));
}

/// 结果原样交给上层（§3.11 的 G10）：rmcp 按类型解析会丢掉不认识的字段（`toolResult`、资源的自定义键），结果的转换
/// （ext.md §3.5、§3.7）要看它们。去掉原样交出时这里失败。
#[tokio::test]
async fn results_reach_the_caller_as_the_server_sent_them() {
    let legacy = json!({"content": [{"type": "text", "text": "c"}], "toolResult": "legacy"});
    let listed = json!({"resources": [{"uri": "memo://one", "name": "one", "vendorKey": "kept"}]});
    let failed = json!({"isError": true, "error": "kaboom"});
    let (call, list, error) = (legacy.clone(), listed.clone(), failed.clone());
    let fake = Arc::new(FakeMcp::default().with(
        "s",
        server(json!({}), None, move |method, params| match method {
            "tools/call" if params["name"] == "legacy" => Some(Ok(call.clone())),
            "tools/call" => Some(Ok(error.clone())),
            "resources/list" => Some(Ok(list.clone())),
            _ => None,
        }),
    ));
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let long = Duration::from_secs(5);
    let request =
        |method: &'static str, params: Value| client.request(method, params, long, &never);
    assert_eq!(
        request("tools/call", json!({"name": "legacy"})).await,
        Ok(legacy)
    );
    assert_eq!(
        request("tools/call", json!({"name": "other"})).await,
        Ok(failed)
    );
    assert_eq!(request("resources/list", json!({})).await, Ok(listed));
}

#[tokio::test]
async fn server_requests_notifications_and_close() {
    let fake = Arc::new(FakeMcp::default().with("s", server(json!({}), None, |_, _| None)));
    let scope = TaskScope::root("test", |_| {});
    let (client, mut notices) = connected(&fake, &scope).await;
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "id": 7, "method": "roots/list"}),
    );
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "id": 8, "method": "sampling/createMessage"}),
    );
    // G12：rmcp 缺省的回调对 elicitation 回 `decline`；前端没声明 elicitation（这里不给问用户的端口）时照旧回 Method not found
    // （声明了的由适配器截下，asks_tests）。
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "id": 9, "method": "elicitation/create", "params": {"message": "?"}}),
    );
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
    );
    assert_eq!(
        notices.recv().await,
        Some(Notice::ListChanged("tools".to_owned()))
    );
    // 回答在后台发（ext.md §3.10.11），按到达等。
    assert_eq!(
        fake.arrival(|message| message["id"] == 7).await,
        json!({"jsonrpc": "2.0", "id": 7, "result": {"roots": [{"uri": "file:///Users/dev/project"}]}})
    );
    for (id, method) in [(8, "sampling/createMessage"), (9, "elicitation/create")] {
        assert_eq!(
            fake.arrival(|message| message["id"] == id).await,
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}})
        );
    }
    // 关闭：等待中的请求失败，之后的请求立即失败。
    let waiting = pending(&client);
    id_of(&fake, "hang").await;
    fake.drop_server("s", true);
    assert!(matches!(waiting.await.unwrap().1, Err(RpcError::Closed(_))));
    assert_eq!(
        notices.recv().await,
        Some(Notice::Closed {
            reason: Some("gone".to_owned()),
            session_expired: true
        })
    );
    assert!(client.is_closed());
    assert_eq!(
        client
            .request(
                "late",
                json!({}),
                Duration::from_secs(5),
                &CancellationToken::new()
            )
            .await,
        Err(RpcError::Closed("connection closed".to_owned()))
    );
}
