//! streamable HTTP 上请求的回复不会来了：传输送出 `ReplyLost`（ext.md §3.10.6 的“续不上时”与“POST 的响应里没有
//! 回复”）。夹具（`resume_fixture`、`call_after_initialize`）在上一级的 `mcp_tests.rs`。

use super::*;

/// 缺陷（v5r-mcp3 线，ext.md §3.10.6 的“续不上时”）：POST 的流在回复之前结束、没给事件 ID 时，传输以前只停止跟读，
/// 请求等到它自己的期限（工具调用缺省约 27.8 小时）。现在先送出这个流已读到的消息，再送 `ReplyLost`。
#[tokio::test]
async fn a_stream_that_ends_without_an_event_id_loses_its_reply() {
    let post = "data: {\"jsonrpc\": \"2.0\", \"method\": \"notifications/message\"}\n\n";
    let (base, seen, server) = resume_fixture(sse_reply(post), None, Vec::new()).await;
    let (_transport, _dir, _id, mut events) = call_after_initialize(&base).await;
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Message {
            message: json!({"jsonrpc": "2.0", "method": "notifications/message"})
        }
    );
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::ReplyLost {
            id: json!(1),
            reason: "MCP response stream ended before the reply, with no event ID to resume from"
                .to_owned()
        }
    );
    assert!(seen.lock().unwrap().is_empty());
    server.abort();
}

/// 续读的 GET 不是 2xx（这里是 SDK 服务器在旧流还开着时回的 409）：请求的回复不会来了，`ReplyLost` 带状态与响应体
/// （ext.md §3.10.6）。
#[tokio::test]
async fn a_refused_resume_loses_the_reply() {
    let (base, _seen, server) = resume_fixture(
        sse_reply("id: e1\nretry: 10\ndata:\n\n"),
        None,
        vec![reply(
            "409 Conflict",
            "",
            "Conflict: Stream already has an active connection",
        )],
    )
    .await;
    let (_transport, _dir, _id, mut events) = call_after_initialize(&base).await;
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::ReplyLost {
            id: json!(1),
            reason: "MCP response stream could not be resumed: HTTP 409: Conflict: Stream already has an active connection"
                .to_owned()
        }
    );
    server.abort();
}

/// 续读的 GET 出错（服务器收了请求、不回就断开）：同上，原因是 net 的错误（ext.md §3.10.6）。
#[tokio::test]
async fn a_failed_resume_loses_the_reply() {
    let (base, _seen, server) = resume_fixture(
        sse_reply("id: e1\nretry: 10\ndata:\n\n"),
        None,
        vec![String::new()],
    )
    .await;
    let (_transport, _dir, _id, mut events) = call_after_initialize(&base).await;
    let lost = next(&mut events).await;
    let McpTransportEvent::ReplyLost { id, reason } = &lost else {
        panic!("{lost:?}");
    };
    assert_eq!(id, &json!(1));
    assert!(
        reason.starts_with("MCP response stream could not be resumed: "),
        "{reason}"
    );
    server.abort();
}

/// 缺陷（v5r-mcp4 线，ext.md §3.10.6 的“POST 的响应里没有回复”）：对请求回 202 时传输以前什么也不送，请求等到它自己
/// 的期限。规范只许对通知与回答回 202，对请求 MUST 回 JSON 或 SSE、监听流上 MUST NOT 发回复，所以回复不会来了：现在
/// 立即送 `ReplyLost`。对服务器请求的回答回 202 是合法的，不送（这里它与请求 1 同 ID）。
#[tokio::test]
async fn a_request_answered_with_202_loses_its_reply() {
    let accepted = "MCP server answered the request with 202 Accepted, which carries no reply";
    let (base, _seen, server) =
        resume_fixture(reply("202 Accepted", "", ""), None, Vec::new()).await;
    let (transport, _dir, id, mut events) = call_after_initialize(&base).await;
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::ReplyLost {
            id: json!(1),
            reason: accepted.to_owned()
        }
    );
    // 回答在请求 2 之前发出：它要是被当成请求，它的 `ReplyLost` 会先到。
    let answer = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
    transport.send(&id, answer).await.unwrap();
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call"});
    transport.send(&id, call).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::ReplyLost {
            id: json!(2),
            reason: accepted.to_owned()
        }
    );
    server.abort();
}

/// 缺陷（同上）：对请求回的内容类型既不是 JSON 也不是 SSE（含没有内容类型）时，传输以前丢掉响应体、什么也不送。SDK
/// 在这里让请求立即失败；现在送 `ReplyLost`，原因带内容类型。
#[tokio::test]
async fn a_request_answered_with_another_content_type_loses_its_reply() {
    let unsupported = "MCP server answered the request with an unsupported content type";
    for (call, shown) in [
        (
            reply("200 OK", "content-type: text/html\r\n", "<html></html>"),
            "text/html",
        ),
        (reply("200 OK", "", ""), "none"),
    ] {
        let (base, _seen, server) = resume_fixture(call, None, Vec::new()).await;
        let (_transport, _dir, _id, mut events) = call_after_initialize(&base).await;
        assert_eq!(
            next(&mut events).await,
            McpTransportEvent::ReplyLost {
                id: json!(1),
                reason: format!("{unsupported}: {shown}")
            }
        );
        server.abort();
    }
}

/// 缺陷（同上）：JSON 响应体里没有这个请求的回复时，传输以前只送出里面的消息，请求等到期限。规范要 JSON 体就是这个
/// 请求的回复；现在先送出里面的消息，再送 `ReplyLost`。
#[tokio::test]
async fn a_json_response_without_the_reply_loses_it() {
    let notice = json!({"jsonrpc": "2.0", "method": "notifications/message"});
    let (base, _seen, server) = resume_fixture(
        reply(
            "200 OK",
            "content-type: application/json\r\n",
            &notice.to_string(),
        ),
        None,
        Vec::new(),
    )
    .await;
    let (_transport, _dir, _id, mut events) = call_after_initialize(&base).await;
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Message { message: notice }
    );
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::ReplyLost {
            id: json!(1),
            reason: "MCP server's JSON response did not include the reply".to_owned()
        }
    );
    server.abort();
}
