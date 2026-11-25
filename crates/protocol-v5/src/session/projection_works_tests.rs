//! 投影里的后台工作（protocol-v5.md §20.1、§20.2）：开始 put、结束 remove；由视图建与活会话是同一项；不认识的结束不发；
//! 有后台工作的会话在会话列表里仍是 hot。

use rust_cli_kernel_api::background::BackgroundStart;
use rust_cli_kernel_api::ids::{SessionId, ToolUseId, TurnId};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOrigin, TurnOutcome};
use serde_json::{Value, json};

use super::super::tests::{live, origin, snapshot, view};
use super::*;

fn shell() -> BackgroundStart {
    BackgroundStart {
        id: "b1".to_owned(),
        kind: BackgroundKind::Shell,
        description: "npm test".to_owned(),
        tool_use: Some(ToolUseId::new("toolu_1")),
        child: None,
        started_at: 1_700,
    }
}

fn agent() -> BackgroundStart {
    BackgroundStart {
        id: "agent_1".to_owned(),
        kind: BackgroundKind::Agent,
        description: "Review".to_owned(),
        tool_use: Some(ToolUseId::new("toolu_2")),
        child: Some(SessionId::new("sess_subagent_agent_1")),
        started_at: 1_800,
    }
}

fn works_of(publisher: &Publisher) -> Value {
    snapshot(publisher)["collections"]["works"].clone()
}

#[test]
fn a_started_work_is_put_and_its_end_removes_it() {
    let (_, publisher) = live(vec![
        KernelEvent::BackgroundStarted { work: shell() },
        KernelEvent::BackgroundStarted { work: agent() },
    ]);
    let b1 = publisher.entity(WORKS, "b1").unwrap();
    // `startedAtSeq` 是放进它的那一批的 seq（这一项的 `v`）
    let put_at = publisher.version(WORKS, "b1").unwrap();
    assert_eq!(
        Value::Object(b1.clone().into_iter().collect()),
        json!({
            "kind": "bash",
            "status": "running",
            "title": "npm test",
            "anchorRowId": "toolu_1",
            "startedAtSeq": put_at,
            "startedAt": 1_700,
            "detail": {"cancellable": true},
        })
    );
    let agent_1 = publisher.entity(WORKS, "agent_1").unwrap();
    assert_eq!(agent_1["kind"], "subagent");
    assert_eq!(agent_1["detail"]["childSessionId"], "sess_subagent_agent_1");
    let (_, publisher) = live(vec![
        KernelEvent::BackgroundStarted { work: shell() },
        KernelEvent::BackgroundEnded {
            work: "b1".to_owned(),
        },
    ]);
    assert_eq!(works_of(&publisher), json!([]));
}

/// 由视图建（物化、激活、改写历史）与活会话是同一项：只有 `v` 与 `startedAtSeq`（topic 自己的次序）不同。
#[test]
fn a_view_with_works_builds_the_same_entities() {
    let (_, live_publisher) = live(vec![KernelEvent::BackgroundStarted { work: shell() }]);
    let mut from_view = view(Vec::new(), Vec::new());
    from_view.works = vec![shell()];
    let (_, content) = Projection::build(&from_view, origin(true));
    let mut built = Publisher::new("t", "e");
    built.adopt(content);
    let strip = |publisher: &Publisher| {
        let mut work = publisher.entity(WORKS, "b1").unwrap().clone();
        work.remove("startedAtSeq");
        work
    };
    assert_eq!(strip(&live_publisher), strip(&built));
    assert_eq!(built.entity(WORKS, "b1").unwrap()["startedAtSeq"], 0);
}

#[test]
fn an_end_the_projection_never_saw_sends_nothing() {
    let (mut projection, publisher) = live(Vec::new());
    let event = SequencedEvent {
        seq: 1,
        event: KernelEvent::BackgroundEnded {
            work: "b9".to_owned(),
        },
        revision: 0,
    };
    let batch = projection.apply(&publisher, &event);
    let mut publisher = publisher;
    assert!(!publisher.publish(batch));
}

/// 04 §4.1：会话仍 hot（有后台工作）时回合结束是 `phase` 的变化，行留在 `live` 里；后台工作结束才去掉。
#[test]
fn a_session_with_running_work_stays_hot_after_its_turn() {
    let turn = TurnId::new("turn_1");
    let (projection, _) = live(vec![
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
            origin: TurnOrigin::Input,
            started_at: 1,
            works: Vec::new(),
        },
        KernelEvent::BackgroundStarted { work: shell() },
    ]);
    let running = projection.hot(None).map(|hot| live::row(&hot)).unwrap();
    assert_eq!(running["phase"], "running");
    assert_eq!(running["hasBackgroundWork"], true);
    let (projection, _) = live(vec![
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
            origin: TurnOrigin::Input,
            started_at: 1,
            works: Vec::new(),
        },
        KernelEvent::BackgroundStarted { work: shell() },
        KernelEvent::TurnFinished {
            turn: turn.clone(),
            outcome: TurnOutcome::Success,
            usage: None,
            time: super::super::kit::ENDED,
            files: Vec::new(),
        },
    ]);
    let after = projection.hot(None).map(|hot| live::row(&hot)).unwrap();
    assert_eq!(after["phase"], "completed");
    assert_eq!(after["runtime"], "ready");
    assert_eq!(after["hasBackgroundWork"], true);
    let (projection, _) = live(vec![
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
            origin: TurnOrigin::Input,
            started_at: 1,
            works: Vec::new(),
        },
        KernelEvent::BackgroundStarted { work: shell() },
        KernelEvent::TurnFinished {
            turn,
            outcome: TurnOutcome::Success,
            usage: None,
            time: super::super::kit::ENDED,
            files: Vec::new(),
        },
        KernelEvent::BackgroundEnded {
            work: "b1".to_owned(),
        },
    ]);
    assert!(projection.hot(None).is_none());
}
