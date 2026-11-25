//! 回合记录带这一轮主循环里的压缩（docs/specs/rust-cli/session-lifecycle.md §10.52）：开始记一项并发事件，成功、失败、停止写结局，
//! 回合结束时与结局同一次落库；视图在跑时带没有结局的那一项。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::compaction::{CompactProgress, LoopCompaction, LoopCompactionEnd};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor,
    OrchestrationError,
};
use rust_cli_kernel_api::session_api::{Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::{Fact, TurnRecord, entry_kinds};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio_util::sync::CancellationToken;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 会话 actor 的时钟（[`Setup::new`] 缺省固定在 1000）。
const NOW: u64 = 1_000;
const FAILURE: &str = "summary failed";

/// 主循环的压缩器每一次怎么走。
#[derive(Clone, Copy)]
enum Step {
    /// 说开始，交出边界与摘要。
    Succeed,
    /// 说开始，再说失败（回合照常继续）。
    Fail,
    /// 说开始，等到中止（像真的压缩器：中止不说失败）。
    Hang,
}

/// 发请求前按剧本自动压缩一次（之后不再压缩）。
struct LoopCompactor(Mutex<Vec<Step>>);

impl LoopCompactor {
    fn new(step: Step) -> Arc<Self> {
        Arc::new(Self(Mutex::new(vec![step])))
    }
}

fn result() -> CompactResult {
    CompactResult {
        boundary: CompactBoundary {
            id: MessageId::new("msg_boundary"),
            trigger: CompactTrigger::Auto,
            pre_tokens: 10,
            summary: MessageId::new("msg_summary"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
        summary: UserMessage {
            id: MessageId::new("msg_summary"),
            content: vec![ContentBlock::Text {
                text: "SUMMARY".to_owned(),
            }],
            origin: UserOrigin::CompactSummary,
        },
        restored: Vec::new(),
    }
}

#[async_trait]
impl Compactor for LoopCompactor {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        !self.0.lock_unpoisoned().is_empty()
    }

    async fn compact_metered(
        &self,
        _request: CompactRequest,
        _cancel: CancellationToken,
        _meter: &dyn rust_cli_kernel_api::compaction::CompactMeter,
    ) -> Result<CompactResult, OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "no reactive compaction".to_owned(),
        })
    }

    async fn auto_compact_with(
        &self,
        _request: CompactRequest,
        _tracking: &mut CompactTracking,
        cancel: CancellationToken,
        progress: &dyn CompactProgress,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        let Some(step) = self.0.lock_unpoisoned().pop() else {
            return Ok(AutoCompactOutcome::NotNeeded);
        };
        progress.started(CompactTrigger::Auto).await;
        Ok(match step {
            Step::Succeed => AutoCompactOutcome::Compacted {
                result: Box::new(result()),
            },
            Step::Fail => {
                progress
                    .failed(CompactTrigger::Auto, FAILURE.to_owned())
                    .await;
                AutoCompactOutcome::Failed {
                    message: FAILURE.to_owned(),
                }
            }
            Step::Hang => {
                cancel.cancelled().await;
                AutoCompactOutcome::Failed {
                    message: "aborted".to_owned(),
                }
            }
        })
    }

    async fn auto_compact(
        &self,
        _request: CompactRequest,
        _tracking: &mut CompactTracking,
        _cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        unreachable!("the main loop compacts with progress")
    }
}

async fn session(step: Step) -> Built {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut setup = Setup::new(model);
    setup.loop_compactor = LoopCompactor::new(step);
    setup.build().await
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
            } if user.origin == UserOrigin::Human => Some(user.id.clone()),
            _ => None,
        })
        .unwrap()
}

/// 事件里 `CompactionRecorded` 的位置与它的值。
fn recorded(events: &[KernelEvent]) -> Vec<(usize, usize, LoopCompaction)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match event {
            KernelEvent::CompactionRecorded {
                index, compaction, ..
            } => Some((at, *index, compaction.clone())),
            _ => None,
        })
        .collect()
}

fn position(events: &[KernelEvent], wanted: impl Fn(&KernelEvent) -> bool) -> usize {
    events.iter().position(wanted).unwrap()
}

fn compaction(after: &MessageId, ended: Option<LoopCompactionEnd>) -> LoopCompaction {
    LoopCompaction {
        trigger: CompactTrigger::Auto,
        started_at: NOW,
        after: after.clone(),
        ended,
    }
}

/// 开始：用户消息之后记一项（触发、actor 的时钟、这时历史的最后一条消息、没有结局）；成功：边界提交之后写 `Succeeded`。回合结束时
/// 回合记录与结局同一次落库，带着它。
#[tokio::test]
async fn a_main_loop_compaction_is_recorded_from_its_start_to_its_success() {
    let built = session(Step::Succeed).await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hi")).await;
    let seen = until(&mut events, finished).await;
    let user = user_message(&seen);
    let recorded = recorded(&seen);
    assert_eq!(
        recorded
            .iter()
            .map(|(_, index, compaction)| (*index, compaction.clone()))
            .collect::<Vec<_>>(),
        vec![
            (0, compaction(&user, None)),
            (0, compaction(&user, Some(LoopCompactionEnd::Succeeded))),
        ],
        "{seen:?}"
    );
    let started = position(&seen, |event| {
        matches!(event, KernelEvent::CompactStarted { .. })
    });
    let boundary = position(
        &seen,
        |event| matches!(event, KernelEvent::MessageDone { message } if message.id().as_str() == "msg_boundary"),
    );
    assert!(started < recorded[0].0 && recorded[0].0 < boundary);
    assert!(boundary < recorded[1].0);
    let stored = stored_turns(&built);
    assert_eq!(
        stored.last().map(|record| record.compactions.clone()),
        Some(vec![compaction(&user, Some(LoopCompactionEnd::Succeeded))])
    );
}

/// 开始之后失败：`CompactFailed` 之后写 `Failed`；回合照常完成，记录里是失败的那一项。
#[tokio::test]
async fn a_failed_main_loop_compaction_is_recorded_and_the_turn_goes_on() {
    let built = session(Step::Fail).await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hi")).await;
    let seen = until(&mut events, finished).await;
    let user = user_message(&seen);
    let recorded = recorded(&seen);
    assert_eq!(
        recorded.last().map(|(_, _, compaction)| compaction.clone()),
        Some(compaction(&user, Some(LoopCompactionEnd::Failed)))
    );
    let failed = position(&seen, |event| {
        matches!(event, KernelEvent::CompactFailed { .. })
    });
    assert!(failed < recorded[1].0);
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: rust_cli_kernel_api::session_api::TurnOutcome::Success,
            ..
        })
    ));
    assert_eq!(
        stored_turns(&built)
            .last()
            .map(|record| record.compactions.clone()),
        Some(vec![compaction(&user, Some(LoopCompactionEnd::Failed))])
    );
}

/// 压缩中停止：视图的在跑回合的记录带没有结局的那一项；回合结束时写 `Cancelled`，`CompactionRecorded` 在 `TurnFinished` 之前，记录落库带它。
#[tokio::test]
async fn a_stopped_main_loop_compaction_is_cancelled_before_the_turn_ends() {
    let built = session(Step::Hang).await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hi")).await;
    let started = until(&mut events, |event| {
        matches!(event, KernelEvent::CompactionRecorded { .. })
    })
    .await;
    let user = user_message(&started);
    let view = built.session.view().await.unwrap();
    assert_eq!(
        view.turns.last().map(|record| record.compactions.clone()),
        Some(vec![compaction(&user, None)])
    );
    built
        .session
        .send(envelope("c2", Command::Interrupt { target: None }))
        .await;
    let seen = until(&mut events, finished).await;
    let recorded = recorded(&seen);
    assert_eq!(
        recorded
            .iter()
            .map(|(_, index, compaction)| (*index, compaction.clone()))
            .collect::<Vec<_>>(),
        vec![(0, compaction(&user, Some(LoopCompactionEnd::Cancelled)))]
    );
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: rust_cli_kernel_api::session_api::TurnOutcome::Interrupted,
            ..
        })
    ));
    assert_eq!(
        stored_turns(&built)
            .last()
            .map(|record| record.compactions.clone()),
        Some(vec![compaction(&user, Some(LoopCompactionEnd::Cancelled))])
    );
}

/// `/compact` 的回合不经主循环：不记这一项、不发 `CompactionRecorded`（它的开始与结局就是回合的）。
#[tokio::test]
async fn a_compact_turn_records_no_main_loop_compaction() {
    let built = session(Step::Succeed).await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(envelope("k1", Command::Compact { instructions: None }))
        .await;
    let seen = until(&mut events, finished).await;
    assert!(recorded(&seen).is_empty(), "{seen:?}");
    assert!(
        stored_turns(&built)
            .iter()
            .all(|record| record.compactions.is_empty())
    );
}
