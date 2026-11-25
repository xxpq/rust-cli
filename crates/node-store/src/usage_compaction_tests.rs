//! 手动 `/compact` 是自己的一轮（usage.md §2.1.3，照 Node 的 `executeManualCompact`）：摘要请求的行与回合的行都落在压缩回合上，
//! 父消息与回合的输入为空；之前那一轮的行不动。

use rust_cli_kernel_api::ids::{MessageId, SessionId, TurnId};
use rust_cli_kernel_api::message::{StopReason, Usage};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::json;

use crate::tests::{TestStore, batch, started, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

async fn commit(t: &TestStore, facts: Vec<Fact>) {
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
}

/// 一次摘要请求（没有首字，同压缩器交出的计量）。
fn summary(message: &str, started_at_ms: u64, status: CallStatus) -> ModelCall {
    ModelCall {
        stop_reason: Some(StopReason::EndTurn),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }),
        ..ModelCall::new(
            MessageId::new(message),
            QuerySource::Compact,
            ModelSelection::new("anthropic", rust_cli_base::models::GLM_5_3),
            started_at_ms,
            started_at_ms + 400,
            status,
        )
    }
}

#[tokio::test]
async fn manual_compaction_rows_are_their_own_turn() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "hello")]).await;
    commit(
        &t,
        vec![Fact::TurnEnded {
            status: CallStatus::Completed,
        }],
    )
    .await;
    let turn = TurnId::new("turn_compact");
    for (message, at, status) in [
        ("msg_s1", 5_000, CallStatus::Error),
        ("msg_s2", 6_000, CallStatus::Completed),
    ] {
        commit(
            &t,
            vec![Fact::CompactionCalled {
                call: summary(message, at, status),
                turn: turn.clone(),
            }],
        )
        .await;
    }
    t.source.at(7_000);
    commit(
        &t,
        vec![Fact::CompactionEnded {
            turn: turn.clone(),
            started_at_ms: 4_900,
            status: CallStatus::Completed,
        }],
    )
    .await;
    assert_eq!(
        t.rows(
            "select id, query_source, turn_id, parent_user_message_id, status
               from model_usage order by started_at"
        ),
        vec![
            vec![
                json!("usage_model_compact_msg_s1_0"),
                json!("compact"),
                json!("turn_compact"),
                json!(null),
                json!("error")
            ],
            vec![
                json!("usage_model_compact_msg_s2_0"),
                json!("compact"),
                json!("turn_compact"),
                json!(null),
                json!("completed")
            ],
        ]
    );
    // 压缩回合一行：没有输入消息，开始是压缩开始的时刻，计数与 token 是这两次请求；之前那一轮不数它们。
    assert_eq!(
        t.rows(
            "select user_message_id, status, started_at, completed_at, duration_ms,
                    model_request_count, tool_call_count, input_tokens, output_tokens
               from turn_usage where turn_id = 'turn_compact'"
        ),
        vec![vec![
            json!(null),
            json!("completed"),
            json!(4_900),
            json!(7_000),
            json!(2_100),
            json!(2),
            json!(0),
            json!(20),
            json!(10)
        ]]
    );
    assert_eq!(
        t.rows("select model_request_count from turn_usage where turn_id != 'turn_compact'"),
        vec![vec![json!(0)]]
    );
}
