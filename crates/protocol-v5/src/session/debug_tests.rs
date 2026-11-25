//! 开发者面板的调试记录（protocol-v5.md §43.2）：请求结束一条网络条目、完成的主回合另记一轮与缓存累计；重试、停顿各一条；超过上限丢最旧的。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::Usage;
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::{ModelError, ModelNotice, QuerySource};
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;
use serde_json::json;

use super::*;

const TRACE: &str = "trace_1";

fn event(seq: u64, event: KernelEvent) -> SequencedEvent {
    SequencedEvent {
        seq,
        event,
        revision: 0,
    }
}

/// 1 000 ms 开始、1 400 ms 首个内容、3 400 ms 结束的请求：输出 120（生成 2 s，60 token/s）。
fn call(id: &str, source: QuerySource, status: CallStatus) -> ModelCall {
    ModelCall {
        first_content_at_ms: Some(1_400),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 120,
            cache_creation_input_tokens: 20,
            cache_read_input_tokens: 70,
        }),
        ..ModelCall::new(
            MessageId::new(id),
            source,
            selection(),
            1_000,
            3_400,
            status,
        )
    }
}

fn called(seq: u64, call: ModelCall) -> SequencedEvent {
    event(seq, KernelEvent::ModelCalled { call })
}

fn log() -> DebugLog {
    let mut log = DebugLog::default();
    log.traced(TRACE);
    log
}

#[test]
fn a_completed_main_request_is_a_network_entry_and_a_round() {
    let mut log = log();
    log.observe(&called(
        3,
        call("msg_1", QuerySource::MainThread, CallStatus::Completed),
    ));
    let snapshot = log.snapshot("ses_1");
    let pick = selection();
    assert_eq!(
        snapshot["networkEntries"],
        json!([{"eventKey": "3", "traceId": TRACE, "recordedAt": 3400.0, "statusType": "model_request_completed",
                "requestId": "msg_1", "providerId": pick.provider, "modelId": pick.model, "querySource": "main_turn",
                "durationMs": 2400.0, "requestHeaders": {}, "responseHeaders": {}, "requestHeaderCount": 0.0,
                "responseHeaderCount": 0.0}])
    );
    // 输入是归一化的：10 + 缓存读 70 + 缓存写 20
    assert_eq!(
        snapshot["rounds"],
        json!([{"eventKey": "3", "requestId": "msg_1", "requestIndex": 1.0, "recordedAt": 3400.0,
                "usage": {"inputTokens": 100.0, "outputTokens": 120.0, "totalTokens": 220.0, "cachedInputTokens": 70.0,
                          "cachedWriteInputTokens": 20.0},
                "hitRate": 0.7, "generationDurationMs": 2000.0, "tokensPerSecond": 60.0}])
    );
    assert_eq!(
        snapshot["cache"],
        json!({"hitRateRequestCount": 1.0, "totalInputTokens": 100.0, "totalCacheReadTokens": 70.0, "hitRate": 0.7})
    );
    assert_eq!(snapshot["sessionId"], json!("ses_1"));
}

#[test]
fn other_requests_and_notices_are_network_entries_only() {
    let mut log = log();
    let mut failed = call("msg_1", QuerySource::MainThread, CallStatus::Error);
    failed.error = Some(ModelError::Overloaded {
        message: "busy".to_owned(),
    });
    log.observe(&called(1, failed));
    log.observe(&called(
        2,
        call("msg_2", QuerySource::MainThread, CallStatus::Cancelled),
    ));
    log.observe(&called(
        3,
        call("msg_3", QuerySource::Compact, CallStatus::Completed),
    ));
    let retrying = ModelNotice::Retrying {
        attempt: 2,
        max_retries: 10,
        delay_ms: 500,
        error: ModelError::Overloaded {
            message: "busy".to_owned(),
        },
    };
    log.observe(&event(
        4,
        KernelEvent::ModelNotice {
            notice: retrying,
            at: 5_000,
        },
    ));
    log.observe(&event(
        5,
        KernelEvent::ModelNotice {
            notice: ModelNotice::Stalled { retry_in_ms: 1 },
            at: 6_000,
        },
    ));
    log.observe(&event(
        6,
        KernelEvent::ModelNotice {
            notice: ModelNotice::Resumed,
            at: 7_000,
        },
    ));
    let snapshot = log.snapshot("ses_1");
    let entries = snapshot["networkEntries"].as_array().unwrap();
    let kinds: Vec<_> = entries
        .iter()
        .map(|entry| entry["statusType"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "model_request_failed",
            "model_request_failed",
            "model_request_completed",
            "model_retry_scheduled",
            "model_stream_stalled"
        ]
    );
    assert_eq!(entries[0]["reason"], json!(REASON_FAILED));
    assert_eq!(entries[0]["message"], json!("model overloaded: busy"));
    assert_eq!(entries[1]["reason"], json!(REASON_CANCELLED));
    assert_eq!(entries[2]["querySource"], json!("compact"));
    assert_eq!(
        (
            &entries[3]["attempt"],
            &entries[3]["maxAttempts"],
            &entries[3]["delayMs"],
            &entries[3]["recordedAt"]
        ),
        (&json!(2.0), &json!(10.0), &json!(500.0), &json!(5000.0))
    );
    // 只有完成的主回合请求是一轮
    assert_eq!(snapshot["rounds"], json!([]));
    assert_eq!(snapshot["cache"], json!(null));
}

#[test]
fn the_record_keeps_the_latest_entries_and_rounds() {
    let mut log = log();
    let total = DEBUG_ROUNDS + 1;
    for seq in 1..=total as u64 {
        log.observe(&called(
            seq,
            call(
                &format!("msg_{seq}"),
                QuerySource::MainThread,
                CallStatus::Completed,
            ),
        ));
    }
    let snapshot = log.snapshot("ses_1");
    let rounds = snapshot["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), DEBUG_ROUNDS);
    assert_eq!(rounds[0]["requestId"], json!("msg_2"));
    let entries = snapshot["networkEntries"].as_array().unwrap();
    assert_eq!(entries.len(), DEBUG_NETWORK_ENTRIES);
    assert_eq!(
        entries[0]["requestId"],
        json!(format!("msg_{}", total - DEBUG_NETWORK_ENTRIES + 1))
    );
    // 缓存累计全部的轮（不随条目丢掉）
    assert_eq!(
        snapshot["cache"]["hitRateRequestCount"],
        json!(total as f64)
    );
}

#[test]
fn an_empty_record_has_no_cache() {
    assert_eq!(
        DebugLog::default().snapshot("ses_1"),
        json!({"sessionId": "ses_1", "rounds": [], "networkEntries": [], "cache": null})
    );
}
