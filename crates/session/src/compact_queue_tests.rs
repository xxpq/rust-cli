//! v5 的压缩（session-lifecycle.md §10.23，02 §4.2、R21）：回合在跑或队列扣住时排进队列，按队列顺序开；一次只有一个压缩。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{CommandId, MessageId, TurnId};
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    CompactRequest, CompactResult, Compactor, OrchestrationError,
};
use rust_cli_kernel_api::session_api::{
    Ack, Command, KernelEvent, QueueHold, Running, SequencedEvent, SessionHandle, TurnOrigin,
};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 每次压缩给出新的边界；`gate` 有值时每次都等它放行。`models` 记下每次压缩用的模型。
#[derive(Default)]
struct Compacting {
    calls: AtomicU64,
    gate: Option<Arc<Notify>>,
    models: std::sync::Mutex<Vec<ModelSelection>>,
}

#[async_trait]
impl Compactor for Compacting {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        false
    }

    async fn compact_metered(
        &self,
        request: CompactRequest,
        cancel: CancellationToken,
        _meter: &dyn rust_cli_kernel_api::compaction::CompactMeter,
    ) -> Result<CompactResult, OrchestrationError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.models.lock().unwrap().push(request.selection);
        if let Some(gate) = &self.gate {
            tokio::select! {
                () = gate.notified() => {}
                () = cancel.cancelled() => return Err(OrchestrationError::Failed { message: "cancelled".to_owned() }),
            }
        }
        Ok(CompactResult {
            boundary: CompactBoundary {
                id: MessageId::new(format!("msg_boundary_{n}")),
                trigger: CompactTrigger::Manual,
                pre_tokens: 7,
                summary: MessageId::new(format!("msg_summary_{n}")),
                preserved: None,
                discovered_tools: Vec::new(),
            },
            summary: UserMessage {
                id: MessageId::new(format!("msg_summary_{n}")),
                content: vec![ContentBlock::Text {
                    text: "summary".to_owned(),
                }],
                origin: UserOrigin::CompactSummary,
            },
            restored: Vec::new(),
        })
    }
}

fn compact(id: &str) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(id, Command::CompactOrQueue { selection: None })
}

/// 这一次提交的模型（不同于会话缺省的模型）。
fn chosen() -> ModelSelection {
    ModelSelection::new("provider-b".to_owned(), "model-b".to_owned())
}

fn compact_with(id: &str) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::CompactOrQueue {
            selection: Some(Box::new(chosen())),
        },
    )
}

/// 压缩用这一次提交的模型（session-lifecycle.md §10.30，照 v4）：空闲时现在开，先写成会话的模型（`ModelChanged`），摘要请求按它发。
#[tokio::test]
async fn an_idle_compaction_runs_with_the_model_it_was_sent_with() {
    let compactor = Arc::new(Compacting::default());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["one"])]));
    let mut setup = Setup::new(model);
    setup.compactor = compactor.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    assert!(matches!(
        built.session.send(compact_with("k1")).await,
        Ack::Accepted { .. }
    ));
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(
        |event| matches!(event, KernelEvent::ModelChanged { selection } if *selection == chosen())
    ));
    assert_eq!(*compactor.models.lock().unwrap(), [chosen()]);
    assert_eq!(built.session.view().await.unwrap().selection, chosen());
}

/// 排进队列的压缩带着它的模型（执行配置只有模型）；回合在跑时会话的模型不变，轮到它时先换再压。
#[tokio::test]
async fn a_queued_compaction_switches_the_model_when_it_runs() {
    let compactor = Arc::new(Compacting::default());
    let (built, gate, mut events, _t1) = running(compactor.clone(), false).await;
    let before = built.session.view().await.unwrap().selection;
    assert!(matches!(
        built.session.send(compact_with("k1")).await,
        Ack::Accepted { .. }
    ));
    let view = built.session.view().await.unwrap();
    assert_eq!(view.selection, before);
    let queued = view.queue.iter().find(|item| item.compact).unwrap();
    assert_eq!(queued.extras.config.selection.as_deref(), Some(&chosen()));
    assert_eq!(queued.extras.config.mode, None);
    gate.notify_one();
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    assert_eq!(*compactor.models.lock().unwrap(), [chosen()]);
    assert_eq!(built.session.view().await.unwrap().selection, chosen());
}

/// 开过的回合：来源与开它的命令。
fn starts(events: &[KernelEvent]) -> Vec<(TurnOrigin, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::TurnStarted {
                origin, command, ..
            } => Some((
                *origin,
                command
                    .as_ref()
                    .map(|id| id.as_str().to_owned())
                    .unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

/// T1（c1）在模型请求上停着；`compactor` 是会话的压缩器。
async fn running(
    compactor: Arc<Compacting>,
    hold_queue: bool,
) -> (
    Built,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
    TurnId,
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
    let mut setup = Setup::new(model);
    setup.compactor = compactor;
    setup.hold_queue = hold_queue;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let Some(KernelEvent::TurnStarted { turn, .. }) = seen.last() else {
        unreachable!("until stops at TurnStarted");
    };
    let turn = turn.clone();
    (built, gate, events, turn)
}

/// 回合在跑：压缩排进队列（项的 compact = true），与输入同一个序列，按队列顺序开；排进去不发输入的结局。
#[tokio::test]
async fn a_compaction_waits_in_the_queue_and_runs_in_order() {
    let (built, gate, mut events, _t1) = running(Arc::new(Compacting::default()), false).await;
    built.session.send(submit("c2", "second")).await;
    assert!(matches!(
        built.session.send(compact("k1")).await,
        Ack::Accepted { .. }
    ));
    built.session.send(submit("c3", "third")).await;
    let view = built.session.view().await.unwrap();
    let queued: Vec<(&str, bool)> = view
        .queue
        .iter()
        .map(|item| (item.command.as_str(), item.compact))
        .collect();
    assert_eq!(queued, [("c2", false), ("k1", true), ("c3", false)]);
    gate.notify_one();
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.extend(until(&mut events, finished).await);
    }
    assert_eq!(
        starts(&seen),
        [
            (TurnOrigin::Input, "c2".to_owned()),
            (TurnOrigin::Compaction, "k1".to_owned()),
            (TurnOrigin::Input, "c3".to_owned()),
        ]
    );
    assert!(!seen.iter().any(
        |event| matches!(event, KernelEvent::InputFate { input, .. } if input.as_str() == "k1")
    ));
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::CompactDone { .. }))
    );
}

/// 回合记录带开回合的命令（session-lifecycle.md §10.31）：输入的回合是输入的命令，排队之后开的压缩回合是压缩的命令；库里读回同值。
/// 以前回合记录没有它，冷读还原不出压缩标记是哪条命令的。
#[tokio::test]
async fn turn_records_keep_the_command_that_opened_them() {
    let (built, gate, mut events, _t1) = running(Arc::new(Compacting::default()), false).await;
    assert!(matches!(
        built.session.send(compact("k1")).await,
        Ack::Accepted { .. }
    ));
    gate.notify_one();
    for _ in 0..2 {
        until(&mut events, finished).await;
    }
    let view = built.session.view().await.unwrap();
    let commands: Vec<Option<&str>> = view
        .turns
        .iter()
        .map(|record| record.command.as_ref().map(CommandId::as_str))
        .collect();
    assert_eq!(commands, [Some("c1"), Some("k1")]);
    let loaded = built
        .store
        .load(&rust_cli_kernel_api::ids::SessionId::new("sess_1"))
        .await
        .unwrap();
    assert_eq!(loaded.turns, view.turns);
}

/// 视图带在跑回合的开始（session-lifecycle.md §10.33）：压缩回合的第一条消息（边界）在摘要之后才落库，这之前开始在 `unrecorded` 里，
/// 视图照样给出来源与压缩的命令（落库之后由回合记录给出，见 stop_tests 的 `the_view_names_the_running_turn`）。以前视图只给回合 id。
#[tokio::test]
async fn the_view_carries_the_start_of_a_running_compaction() {
    let gate = Arc::new(Notify::new());
    let compactor = Arc::new(Compacting {
        gate: Some(gate.clone()),
        ..Compacting::default()
    });
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["one"])]));
    let mut setup = Setup::new(model);
    setup.compactor = compactor.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    built.session.send(compact("k1")).await;
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let Some(KernelEvent::TurnStarted {
        turn,
        command,
        origin,
        started_at,
        ..
    }) = seen.last().cloned()
    else {
        unreachable!("until stops at TurnStarted");
    };
    let start = rust_cli_kernel_api::session_api::TurnStart {
        turn,
        command,
        origin,
        started_at,
        works: Vec::new(),
    };
    assert_eq!(start.origin, TurnOrigin::Compaction);
    assert_eq!(start.command.as_ref().map(CommandId::as_str), Some("k1"));
    let view = built.session.view().await.unwrap();
    assert!(
        !view.turns.iter().any(|record| record.turn == start.turn),
        "the compaction has no record before its boundary"
    );
    assert_eq!(view.running, Some(Running::Turn(start)));
    gate.notify_one();
    until(&mut events, finished).await;
    assert_eq!(built.session.view().await.unwrap().running, None);
}

/// 一次只有一个压缩：排着的或在跑的压缩之外再要一次，拒绝（CompactLocked）。
#[tokio::test]
async fn a_second_compaction_is_locked_while_one_waits_or_runs() {
    let compactor = Arc::new(Compacting {
        gate: Some(Arc::new(Notify::new())),
        ..Compacting::default()
    });
    let (built, gate, mut events, _t1) = running(compactor.clone(), false).await;
    assert!(matches!(
        built.session.send(compact("k1")).await,
        Ack::Accepted { .. }
    ));
    let locked = built.session.send(compact("k2")).await;
    assert!(
        matches!(
            locked,
            Ack::Rejected {
                refusal: Refusal::CompactLocked,
                ..
            }
        ),
        "{locked:?}"
    );
    gate.notify_one();
    until(&mut events, finished).await;
    // 压缩在跑（压缩器停着）：同样拒绝
    until(&mut events, |event| {
        matches!(
            event,
            KernelEvent::TurnStarted {
                origin: TurnOrigin::Compaction,
                ..
            }
        )
    })
    .await;
    let locked = built.session.send(compact("k3")).await;
    assert!(
        matches!(
            locked,
            Ack::Rejected {
                refusal: Refusal::CompactLocked,
                ..
            }
        ),
        "{locked:?}"
    );
    if let Some(gate) = &compactor.gate {
        gate.notify_one();
    }
    until(&mut events, finished).await;
    // 压完了：可以再压
    assert!(matches!(
        built.session.send(compact("k4")).await,
        Ack::Accepted { .. }
    ));
}

/// 队列扣住（停止之后）：压缩排到扣住的队列后面，不绕过用户还没决定的输入。
#[tokio::test]
async fn a_held_queue_keeps_the_compaction_behind_the_choice() {
    let (built, _gate, mut events, t1) = running(Arc::new(Compacting::default()), true).await;
    built.session.send(submit("c2", "second")).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: Some(t1) }))
        .await;
    until(&mut events, finished).await;
    assert!(matches!(
        built.session.send(compact("k1")).await,
        Ack::Accepted { .. }
    ));
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue_hold, Some(QueueHold::Stopped));
    assert!(view.running.is_none());
    let queued: Vec<&str> = view
        .queue
        .iter()
        .map(|item| item.command.as_str())
        .collect();
    assert_eq!(queued, ["c2", "k1"]);
}

/// 提升排队的压缩（v5 的 queue.sendNow）：中止指名的回合，压缩先开，之后照常开排着的输入。
#[tokio::test]
async fn a_queued_compaction_can_be_promoted() {
    let (built, _gate, mut events, t1) = running(Arc::new(Compacting::default()), false).await;
    built.session.send(compact("k1")).await;
    built.session.send(submit("c2", "second")).await;
    let promote = envelope(
        "p1",
        Command::Promote {
            command: CommandId::new("k1"),
            target: Some(t1),
        },
    );
    assert!(matches!(
        built.session.send(promote).await,
        Ack::Accepted { .. }
    ));
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.extend(until(&mut events, finished).await);
    }
    assert_eq!(
        starts(&seen),
        [
            (TurnOrigin::Compaction, "k1".to_owned()),
            (TurnOrigin::Input, "c2".to_owned()),
        ]
    );
}

/// 取回排队的压缩：它不是输入，不发输入的结局。
#[tokio::test]
async fn taking_back_a_queued_compaction_has_no_input_fate() {
    let (built, gate, mut events, _t1) = running(Arc::new(Compacting::default()), false).await;
    built.session.send(compact("k1")).await;
    let back = envelope(
        "d1",
        Command::Dequeue {
            command: CommandId::new("k1"),
        },
    );
    assert!(matches!(
        built.session.send(back).await,
        Ack::Accepted { .. }
    ));
    assert!(built.session.view().await.unwrap().queue.is_empty());
    gate.notify_one();
    let seen = until(&mut events, finished).await;
    assert!(!seen.iter().any(
        |event| matches!(event, KernelEvent::InputFate { input, .. } if input.as_str() == "k1")
    ));
}
