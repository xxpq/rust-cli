//! 排队项的编辑锁与整条替换（session-lifecycle.md §10.59，App 的 Composer 编辑态）：锁着的队首不出队、后面的等着；空闲时新的输入排到它后面；
//! 锁只属于一个编辑器；被锁挡住的命令；带锁的整条替换放开锁、接着开它；取空扣住的队列连锁着的一起取走；套用配置跳过锁着的项。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::input_extras::{InputExtras, TurnConfig};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandResult, Delivery, EditGuard, KernelEvent, QueuedEdit, QueuedInput,
    SequencedEvent, SessionHandle,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::turn_execution::TurnExecution;
use tokio::sync::Notify;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

fn lock(
    id: &str,
    command: &str,
    editor: &str,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::LockQueued {
            command: CommandId::new(command),
            editor: editor.to_owned(),
        },
    )
}

fn unlock(id: &str, command: &str) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::UnlockQueued {
            command: CommandId::new(command),
        },
    )
}

fn replace(
    id: &str,
    command: &str,
    edit: QueuedEdit,
    editor: &str,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::EditQueued {
            command: CommandId::new(command),
            edit: Box::new(edit),
            guard: EditGuard::Lock {
                editor: editor.to_owned(),
            },
        },
    )
}

fn refused(ack: &Ack) -> Option<&Refusal> {
    match ack {
        Ack::Rejected { refusal, .. } => Some(refusal),
        _ => None,
    }
}

fn accepted(ack: &Ack) -> bool {
    matches!(
        ack,
        Ack::Accepted {
            result: CommandResult::Done
        }
    )
}

fn unchanged(ack: &Ack) -> bool {
    matches!(
        ack,
        Ack::Accepted {
            result: CommandResult::Unchanged
        }
    )
}

async fn item(built: &Built, command: &str) -> QueuedInput {
    built
        .session
        .view()
        .await
        .unwrap()
        .queue
        .into_iter()
        .find(|item| item.command.as_str() == command)
        .unwrap()
}

async fn queued(built: &Built) -> Vec<String> {
    built
        .session
        .view()
        .await
        .unwrap()
        .queue
        .into_iter()
        .map(|item| item.command.as_str().to_owned())
        .collect()
}

/// T1 在模型请求上停着，排着 `texts` 里的每一条（c2、c3……）；之后的回合各回一句。
async fn queued_behind_t1(
    texts: &[&str],
) -> (
    Built,
    Arc<ScriptedModel>,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
) {
    queued_behind(texts, false).await
}

/// 同 [`queued_behind_t1`]；`hold` 是停止之后扣住队列的开关（App 开着，§10.6）。
async fn queued_behind(
    texts: &[&str],
    hold: bool,
) -> (
    Built,
    Arc<ScriptedModel>,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
) {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![
            text_response(&["one"]),
            text_response(&["two"]),
            text_response(&["three"]),
        ])
        .gated_at(0, gate.clone()),
    );
    let mut setup = Setup::new(model.clone());
    setup.hold_queue = hold;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    for (index, text) in texts.iter().enumerate() {
        built
            .session
            .send(submit(&format!("c{}", index + 2), text))
            .await;
    }
    (built, model, gate, events)
}

/// 队首锁着：T1 结束之后不开它，后面的也等着（不越过它、不与它成批）；空闲时新的输入排到队尾（准入的 `frozen`），不开回合。
/// 带锁的整条替换：正文与配置原位换掉、锁放开，同一步接着开它。
#[tokio::test]
async fn a_locked_head_waits_and_the_locked_replacement_starts_it() {
    let (built, model, gate, mut events) = queued_behind_t1(&["second", "third"]).await;
    let ack = built.session.send(lock("l1", "c2", "editor_a")).await;
    assert!(accepted(&ack), "{ack:?}");
    assert!(item(&built, "c2").await.edit_lock.is_some());
    gate.notify_one();
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert!(
        view.running.is_none(),
        "the locked head must not start a turn"
    );
    assert_eq!(model.requests().len(), 1);
    assert_eq!(queued(&built).await, ["c2", "c3"]);
    // 空闲、队首冻结：新的输入排到队尾，不越过冻结的项
    let ack = built.session.send(submit("c4", "fourth")).await;
    assert!(accepted(&ack), "{ack:?}");
    assert_eq!(queued(&built).await, ["c2", "c3", "c4"]);
    assert!(built.session.view().await.unwrap().running.is_none());
    let selection = ModelSelection::new("provider_b", "model_b");
    let edit = QueuedEdit {
        config: TurnConfig {
            selection: Some(Box::new(selection.clone())),
            mode: Some(PermissionMode::AcceptEdits),
            plan: Some(false),
        },
        ..QueuedEdit::text("edited")
    };
    let ack = built
        .session
        .send(replace("r1", "c2", edit, "editor_a"))
        .await;
    assert!(accepted(&ack), "{ack:?}");
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    until(&mut events, finished).await;
    let sent = serde_json::to_string(&model.requests()[1].messages).unwrap();
    assert!(sent.contains("edited"), "{sent}");
    assert!(!sent.contains("second"), "{sent}");
    let view = built.session.view().await.unwrap();
    assert!(
        view.selection.same_choice(&selection),
        "{:?}",
        view.selection
    );
    assert_eq!(view.mode, PermissionMode::AcceptEdits);
}

/// 锁只属于一个编辑器：同一个编辑器再锁照样接受；别的编辑器加锁、带别的锁替换、不带锁改、取回、挪动、提升都被挡住；谁都能放开；
/// 放开之后再放开是 Unchanged，带原来的锁替换是 EditLockLost。
#[tokio::test]
async fn the_lock_belongs_to_one_editor_and_anyone_can_release_it() {
    let (built, _model, gate, _events) = queued_behind_t1(&["second", "third"]).await;
    assert!(accepted(&built.session.send(lock("l1", "c2", "a")).await));
    assert!(accepted(&built.session.send(lock("l2", "c2", "a")).await));
    let ack = built.session.send(lock("l3", "c2", "b")).await;
    assert_eq!(refused(&ack), Some(&Refusal::QueueItemLocked));
    let ack = built
        .session
        .send(replace("r1", "c2", QueuedEdit::text("x"), "b"))
        .await;
    assert_eq!(refused(&ack), Some(&Refusal::EditLockLost));
    let seen = item(&built, "c2").await;
    let running = built
        .session
        .view()
        .await
        .unwrap()
        .running
        .and_then(|running| running.turn().map(|start| start.turn.clone()));
    let blocked = [
        Command::EditQueued {
            command: CommandId::new("c2"),
            edit: Box::new(QueuedEdit::text("x")),
            guard: EditGuard::Seen {
                expected: Box::new(seen),
            },
        },
        Command::Dequeue {
            command: CommandId::new("c2"),
        },
        Command::MoveQueued {
            command: CommandId::new("c2"),
            before: None,
        },
        Command::Promote {
            command: CommandId::new("c2"),
            target: running,
        },
    ];
    for (index, command) in blocked.into_iter().enumerate() {
        let ack = built
            .session
            .send(envelope(&format!("b{index}"), command))
            .await;
        assert_eq!(refused(&ack), Some(&Refusal::QueueItemLocked), "{ack:?}");
    }
    // 别的项照常：挪 c3 到 c2 之前
    let ack = built
        .session
        .send(envelope(
            "m1",
            Command::MoveQueued {
                command: CommandId::new("c3"),
                before: Some(CommandId::new("c2")),
            },
        ))
        .await;
    assert!(accepted(&ack), "{ack:?}");
    assert_eq!(queued(&built).await, ["c3", "c2"]);
    assert!(accepted(&built.session.send(unlock("u1", "c2")).await));
    assert!(item(&built, "c2").await.edit_lock.is_none());
    assert!(unchanged(&built.session.send(unlock("u2", "c2")).await));
    let ack = built
        .session
        .send(replace("r2", "c2", QueuedEdit::text("x"), "a"))
        .await;
    assert_eq!(refused(&ack), Some(&Refusal::EditLockLost));
    assert!(unchanged(&built.session.send(unlock("u3", "c9")).await));
    let ack = built.session.send(lock("l4", "c9", "a")).await;
    assert_eq!(refused(&ack), Some(&Refusal::NotFound));
    gate.notify_one();
}

/// 放开锁之后空闲就开下一项（锁着的队首只因为锁而没开）。
#[tokio::test]
async fn releasing_the_lock_of_an_idle_session_starts_the_head() {
    let (built, model, gate, mut events) = queued_behind_t1(&["second"]).await;
    built.session.send(lock("l1", "c2", "a")).await;
    gate.notify_one();
    until(&mut events, finished).await;
    assert!(built.session.view().await.unwrap().running.is_none());
    assert!(accepted(&built.session.send(unlock("u1", "c2")).await));
    until(&mut events, finished).await;
    let sent = serde_json::to_string(&model.requests()[1].messages).unwrap();
    assert!(sent.contains("second"), "{sent}");
}

/// 取空扣住的队列（`discardQueue`）连锁着的项一起取走（照 v4：这是用户对整个队列的处置）。
#[tokio::test]
async fn discarding_the_held_queue_takes_the_locked_item_too() {
    let (built, _model, gate, mut events) = queued_behind(&["second"], true).await;
    built.session.send(lock("l1", "c2", "a")).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    gate.notify_one();
    until(&mut events, finished).await;
    let ack = built
        .session
        .send(envelope(
            "c3",
            Command::SubmitDiscardingQueue {
                text: "fresh".to_owned(),
                extras: InputExtras::default(),
                expected_held: None,
            },
        ))
        .await;
    assert!(accepted(&ack), "{ack:?}");
    assert!(queued(&built).await.is_empty());
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::InputFate {
            input,
            fate: rust_cli_kernel_api::session_api::InputFate::Dropped { .. },
            ..
        } if *input == CommandId::new("c2")
    )));
}

/// 只锁排在回合之后的普通文字：排队的压缩与插话锁不住。
#[tokio::test]
async fn a_compaction_or_a_steer_cannot_be_locked() {
    let (built, _model, gate, _events) = queued_behind_t1(&[]).await;
    built
        .session
        .send(envelope("k1", Command::CompactOrQueue { selection: None }))
        .await;
    let ack = built.session.send(lock("l1", "k1", "a")).await;
    assert_eq!(refused(&ack), Some(&Refusal::Unavailable));
    built
        .session
        .send(envelope(
            "s1",
            Command::Submit {
                text: "steer".to_owned(),
                extras: InputExtras::default(),
                delivery: Delivery::Steer,
                target: None,
            },
        ))
        .await;
    let ack = built.session.send(lock("l2", "s1", "a")).await;
    assert_eq!(refused(&ack), Some(&Refusal::Unavailable));
    gate.notify_one();
}

/// 套用配置：锁着的、不在的跳过，其余换模式；再套一次什么都没变 → Unchanged。换了模型的项的执行覆盖与注记作废，模型没变的保留。
#[tokio::test]
async fn applying_a_config_skips_locked_items_and_a_new_model_drops_the_card() {
    let (built, _model, gate, _events) = queued_behind_t1(&["second"]).await;
    let card = ModelSelection::new("provider_card", "model_card");
    let extras = InputExtras {
        config: TurnConfig {
            selection: Some(Box::new(ModelSelection::new("provider_a", "model_a"))),
            ..TurnConfig::default()
        },
        execution: Some(Box::new(TurnExecution {
            selection: card,
            credential: None,
            fallback: None,
        })),
        note: Some(Box::new(serde_json::json!({"card": "c"}))),
        ..InputExtras::default()
    };
    for id in ["c3", "c4"] {
        built
            .session
            .send(envelope(
                id,
                Command::Submit {
                    text: id.to_owned(),
                    extras: extras.clone(),
                    delivery: Delivery::Queue,
                    target: None,
                },
            ))
            .await;
    }
    built.session.send(lock("l1", "c2", "a")).await;
    let guarded = TurnConfig {
        mode: Some(PermissionMode::Guarded),
        ..TurnConfig::default()
    };
    let apply = |id: &str, config: TurnConfig| {
        envelope(
            id,
            Command::ApplyQueuedConfig {
                commands: ["c2", "c3", "c9"].map(CommandId::new).to_vec(),
                config,
            },
        )
    };
    assert!(accepted(
        &built.session.send(apply("a1", guarded.clone())).await
    ));
    assert_eq!(item(&built, "c2").await.extras.config.mode, None);
    let c3 = item(&built, "c3").await;
    assert_eq!(c3.extras.config.mode, Some(PermissionMode::Guarded));
    assert!(c3.extras.execution.is_some(), "the model did not change");
    assert!(unchanged(&built.session.send(apply("a2", guarded)).await));
    let switch = TurnConfig {
        selection: Some(Box::new(ModelSelection::new("provider_b", "model_b"))),
        ..TurnConfig::default()
    };
    assert!(accepted(&built.session.send(apply("a3", switch)).await));
    let c3 = item(&built, "c3").await;
    assert!(
        c3.extras.execution.is_none() && c3.extras.note.is_none(),
        "{c3:?}"
    );
    let c4 = item(&built, "c4").await;
    assert!(c4.extras.execution.is_some(), "c4 was not listed");
    gate.notify_one();
}
