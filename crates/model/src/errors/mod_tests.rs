//! 分类器的入口：结束原因的通用表、传输错误、选厂商之后的路由。

use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::{NetError, ResponseHeaders};
use std::time::SystemTime;

use super::*;

/// 结束原因的通用表（三种协议的值合在一张表里；不认识的是 `Unknown`）。
#[test]
fn finish_reasons_map_to_stop_reasons() {
    for (raw, reason) in [
        ("end_turn", StopReason::EndTurn),
        ("stop", StopReason::EndTurn),
        ("stop_sequence", StopReason::StopSequence),
        ("tool_use", StopReason::ToolUse),
        ("tool_calls", StopReason::ToolUse),
        ("function_call", StopReason::ToolUse),
        ("max_tokens", StopReason::MaxTokens),
        ("length", StopReason::MaxTokens),
        ("max_output_tokens", StopReason::MaxTokens),
        // 修改（provider-errors.md D7 暂定）：以前是 `Unknown`，主循环静默结束。
        (
            "model_context_window_exceeded",
            StopReason::ModelContextWindowExceeded,
        ),
        ("pause_turn", StopReason::PauseTurn),
        ("refusal", StopReason::Refusal),
        ("content_filter", StopReason::Refusal),
        ("compaction", StopReason::Unknown),
    ] {
        assert_eq!(
            classify_finish(Vendor::Generic, raw),
            Finish::Stop(reason),
            "{raw}"
        );
    }
}

/// 传输错误：建连、超时、读写中断是网络错误；TLS 单列（不重试）；无效的请求是参数错误。
#[test]
fn transport_errors_are_classified() {
    let timeout = classify_transport(&NetError::Timeout);
    assert_eq!(timeout.class, Class::Network);
    assert!(matches!(timeout.error, ModelError::Network { .. }));
    let tls = classify_transport(&NetError::Tls("bad certificate".to_owned()));
    assert_eq!(tls.class, Class::TlsFailure);
    assert!(matches!(tls.error, ModelError::Network { .. }));
    let invalid = classify_transport(&NetError::Request("bad url".to_owned()));
    assert_eq!(invalid.class, Class::InvalidRequest);
}

/// 截断与协议违例的形状照旧是 `Protocol`。
#[test]
fn stream_violations_keep_the_protocol_shape() {
    assert_eq!(
        Verdict::truncated().error,
        ModelError::Protocol {
            message: "stream ended before message_stop".to_owned()
        }
    );
    assert_eq!(Verdict::truncated().class, Class::Truncated);
    assert_eq!(Verdict::malformed("x").class, Class::Malformed);
}

/// 智谱的端点上表外的业务码也照原样带上（model.md §5.2）：分类仍由协议层定（400 是参数错误），码给执行覆盖的退回规则判。
#[test]
fn a_zhipu_code_outside_the_table_is_kept() {
    let body = r#"{"error":{"code":"3402","message":"highspeed card is invalid"}}"#;
    let verdict = classify_response(
        Vendor::Zhipu,
        WireProtocol::AnthropicMessages,
        (400, &ResponseHeaders::default()),
        body,
        SystemTime::UNIX_EPOCH,
    );
    assert_eq!(verdict.class, Class::InvalidRequest);
    assert_eq!(verdict.code.as_deref(), Some("3402"));
    let generic = classify_response(
        Vendor::Generic,
        WireProtocol::AnthropicMessages,
        (400, &ResponseHeaders::default()),
        body,
        SystemTime::UNIX_EPOCH,
    );
    assert_eq!(generic.code, None);
}
