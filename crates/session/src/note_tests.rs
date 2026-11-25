//! 输入的注记与执行覆盖的退回事实（docs/specs/rust-cli/session-lifecycle.md §10.48）。

use std::sync::Arc;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::input_extras::{InputExtras, InputRecord};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::session_api::{Ack, Command, Delivery, KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::{Fact, TurnRecord, entry_kinds};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::turn_execution::{
    ExecutionFallback, FallbackRule, FellBack, TurnExecution,
};
use serde_json::{Value, json};

use crate::test_support::{Built, Setup, envelope, finished, until};

fn note() -> Value {
    json!({"schemaVersion": 1, "cardId": "hsc_1", "expiresAt": 2_000})
}

fn submit(text: &str, extras: InputExtras) -> Command {
    Command::Submit {
        text: text.to_owned(),
        extras,
        delivery: Delivery::Queue,
        target: None,
    }
}

fn noted() -> InputExtras {
    InputExtras {
        note: Some(Box::new(note())),
        ..InputExtras::default()
    }
}

/// 落库的输入记录（同一条消息的，按写入顺序；最后一条是现在的值）。
fn stored_inputs(built: &Built) -> Vec<InputRecord> {
    built
        .store
        .facts(built.session.id())
        .into_iter()
        .filter_map(|fact| match fact {
            Fact::Entry { kind, id, data } if kind == entry_kinds::INPUT => {
                InputRecord::from_entry(&id, &data)
            }
            _ => None,
        })
        .collect()
}

fn stored_turns(built: &Built) -> Vec<TurnRecord> {
    built
        .store
        .facts(built.session.id())
        .into_iter()
        .filter_map(|fact| match fact {
            Fact::Entry { kind, id, data } if kind == entry_kinds::TURN => {
                TurnRecord::from_entry(&id, &data)
            }
            _ => None,
        })
        .collect()
}

fn user_message(events: &[KernelEvent]) -> MessageId {
    events
        .iter()
        .find_map(|event| match event {
            KernelEvent::MessageDone {
                message: Message::User(user),
            } => Some(user.id.clone()),
            _ => None,
        })
        .unwrap()
}

/// 注记随开回合的输入落库：输入记录带它，`InputRecorded` 排在这条用户消息的 `MessageDone` 之前，视图也带它。
#[tokio::test]
async fn a_note_is_recorded_with_its_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(envelope("c1", submit("hi", noted())))
        .await;
    let seen = until(&mut events, finished).await;
    let message = user_message(&seen);
    let recorded = seen
        .iter()
        .position(|event| matches!(event, KernelEvent::InputRecorded { record } if record.note == Some(note())))
        .unwrap();
    let done = seen
        .iter()
        .position(|event| {
            matches!(
                event,
                KernelEvent::MessageDone {
                    message: Message::User(_)
                }
            )
        })
        .unwrap();
    assert!(recorded < done, "{seen:?}");
    assert_eq!(
        stored_inputs(&built)
            .last()
            .map(|record| (&record.message, &record.note)),
        Some((&message, &Some(note())))
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(
        view.inputs.last().and_then(|record| record.note.clone()),
        Some(note())
    );
}

/// `PatchNote`：浅合并进注记，同一条输入记录再写一次，发 `InputRecorded`（新值）；视图跟着换。消息不在 → `NotFound`；没有注记 → `Unavailable`。
#[tokio::test]
async fn a_patch_merges_into_the_note_and_is_stored() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["ok"]),
        text_response(&["plain"]),
    ]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(envelope("c1", submit("hi", noted())))
        .await;
    let message = user_message(&until(&mut events, finished).await);
    built
        .session
        .send(envelope("c2", submit("plain", InputExtras::default())))
        .await;
    let plain = user_message(&until(&mut events, finished).await);

    let patch = json!({"savedDurationMs": 900, "highspeedTps": 160});
    let ack = built
        .session
        .send(envelope(
            "c3",
            Command::PatchNote {
                message: message.clone(),
                patch: patch.as_object().unwrap().clone(),
            },
        ))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let merged = json!({"schemaVersion": 1, "cardId": "hsc_1", "expiresAt": 2_000,
        "savedDurationMs": 900, "highspeedTps": 160});
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::InputRecorded { .. })
    })
    .await;
    assert!(
        matches!(seen.last(), Some(KernelEvent::InputRecorded { record }) if record.note == Some(merged.clone()))
    );
    assert_eq!(
        stored_inputs(&built)
            .last()
            .map(|record| (&record.message, &record.note)),
        Some((&message, &Some(merged.clone())))
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(
        view.inputs
            .iter()
            .find(|record| record.message == message)
            .and_then(|record| record.note.clone()),
        Some(merged)
    );

    for (id, missing, refusal) in [
        ("c4", MessageId::new("msg_gone"), Refusal::NotFound),
        ("c5", plain, Refusal::Unavailable),
    ] {
        let ack = built
            .session
            .send(envelope(
                id,
                Command::PatchNote {
                    message: missing,
                    patch: patch.as_object().unwrap().clone(),
                },
            ))
            .await;
        assert!(
            matches!(&ack, Ack::Rejected { refusal: got, .. } if *got == refusal),
            "{ack:?}"
        );
    }
}

/// 执行覆盖退回了：`ExecutionFellBack` 带命中的原因与 actor 的时钟，回合记录跟着记下，回合结束时与结局一起落库。
#[tokio::test]
async fn a_fallback_is_kept_on_the_turn_record() {
    let fell_back = vec![Err(ModelError::ExecutionFellBack {
        reason: "highspeed_card_expired".to_owned(),
        error: Box::new(ModelError::InvalidRequest {
            message: "[3402] card".to_owned(),
        }),
    })];
    let model = Arc::new(ScriptedModel::new(vec![fell_back, text_response(&["ok"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    let extras = InputExtras {
        execution: Some(Box::new(TurnExecution {
            selection: ModelSelection::new("account:zai-highspeed-card", "chosen"),
            credential: Some("card-1".to_owned()),
            fallback: Some(ExecutionFallback {
                rules: vec![FallbackRule {
                    reason: "highspeed_card_expired".to_owned(),
                    provider_code: Some("3402".to_owned()),
                }],
                unavailable: "highspeed_selection_unavailable".to_owned(),
            }),
        })),
        ..noted()
    };
    built
        .session
        .send(envelope("c1", submit("hi", extras)))
        .await;
    let seen = until(&mut events, finished).await;
    let announced: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            KernelEvent::ExecutionFellBack { turn, fell_back } => {
                Some((turn.clone(), fell_back.clone()))
            }
            _ => None,
        })
        .collect();
    let [(turn, fell_back)] = announced.as_slice() else {
        panic!("one fallback expected: {seen:?}");
    };
    assert_eq!(fell_back.reason, "highspeed_card_expired");
    let stored = stored_turns(&built);
    let ended = stored
        .iter()
        .rev()
        .find(|record| record.turn == *turn && record.ended.is_some())
        .unwrap();
    assert_eq!(ended.fell_back.as_ref(), Some(fell_back));
    let view = built.session.view().await.unwrap();
    assert_eq!(
        view.turns
            .iter()
            .find(|record| record.turn == *turn)
            .and_then(|record| record.fell_back.clone()),
        Some(FellBack {
            reason: "highspeed_card_expired".to_owned(),
            at: fell_back.at,
        })
    );
}
