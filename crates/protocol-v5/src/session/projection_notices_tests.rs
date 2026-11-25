//! 只供显示的提示（protocol-v5.md §28.1）：内核的 `SystemNotice` 进 state 的 `notices`，一条一个 id（这一帧的 topic 序号）；回合开始时清空；
//! 由视图建的投影（冷读、激活、改写历史）没有提示——它不落库。

use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::session_api::{
    KernelEvent, NoticeKind, NoticeLevel, TurnOrigin, TurnOutcome,
};
use serde_json::{Value, json};

use super::kit::{apply, live, origin, view};
use super::*;

/// 线上的 state 键（契约 `SESSION_STATE_KEY_SCHEMAS.notices`）。
const NOTICES: &str = "notices";
const CAP: &str = "A hook blocked the turn from ending 8 consecutive times";
const BLOCKED: &str = "steer blocked by hook";

fn started(turn: &str) -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new(turn),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 100,
        works: Vec::new(),
    }
}

fn notice(level: NoticeLevel, text: &str) -> KernelEvent {
    KernelEvent::SystemNotice {
        level,
        text: text.to_owned(),
        kind: None,
    }
}

fn notices(publisher: &Publisher) -> Value {
    publisher
        .state()
        .get(NOTICES)
        .cloned()
        .unwrap_or(Value::Null)
}

/// 修 bug（hooks.md §12 第 2 项）：以前投影把 `SystemNotice` 落进 `_ => {}`，开关开时看不到 Stop hook 到上限的警告。
#[test]
fn a_notice_goes_into_state_with_the_seq_of_its_frame() {
    let (mut projection, mut publisher) = live();
    apply(&mut publisher, &mut projection, vec![started("turn_1")]);
    apply(
        &mut publisher,
        &mut projection,
        vec![notice(NoticeLevel::Warning, CAP)],
    );
    let seq = publisher.cursor().seq;
    assert_eq!(
        notices(&publisher),
        json!([{"id": seq.to_string(), "level": "warning", "text": CAP}])
    );
}

/// 一个工具边界上拦下几条插话：几条都在，按发生的顺序；回合结束不清空，下一个回合开始才清空。
#[test]
fn notices_add_up_within_a_turn_and_the_next_turn_clears_them() {
    let (mut projection, mut publisher) = live();
    apply(
        &mut publisher,
        &mut projection,
        vec![
            started("turn_1"),
            notice(NoticeLevel::Warning, BLOCKED),
            notice(NoticeLevel::Info, CAP),
            KernelEvent::TurnFinished {
                turn: TurnId::new("turn_1"),
                outcome: TurnOutcome::Success,
                usage: None,
                time: super::kit::ENDED,
                files: Vec::new(),
            },
        ],
    );
    let kept = notices(&publisher);
    let items = kept.as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|item| (item["level"].clone(), item["text"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("warning"), json!(BLOCKED)),
            (json!("info"), json!(CAP))
        ]
    );
    assert_ne!(items[0]["id"], items[1]["id"]);
    apply(&mut publisher, &mut projection, vec![started("turn_2")]);
    assert_eq!(notices(&publisher), json!([]));
}

/// 不落库：由视图建的投影（冷读与活会话同一条路）写 `[]`，快照里的键是完整的。
#[test]
fn a_projection_built_from_a_view_has_no_notices() {
    for live in [false, true] {
        let (_, content) = Projection::build(&view(Vec::new(), Vec::new()), origin(live));
        assert_eq!(content.state.get(NOTICES), Some(&json!([])));
    }
}

/// SMALL-3（§33.1）：带种类的提示写 `kind` 与 `params`（界面据它按语言显示整句）：Stop hook 到上限的参数是连续次数与调上限的变量名；
/// 没有种类的一条（插话被拦下）不写这两个键，界面显示原文。以前投影只写原文，中文界面上这条警告是英文。
#[test]
fn a_notice_with_a_kind_carries_the_kind_and_its_params() {
    let (mut projection, mut publisher) = live();
    apply(&mut publisher, &mut projection, vec![started("turn_1")]);
    apply(
        &mut publisher,
        &mut projection,
        vec![
            KernelEvent::SystemNotice {
                level: NoticeLevel::Warning,
                text: CAP.to_owned(),
                kind: Some(NoticeKind::StopHookLimit { blocked: 9 }),
            },
            notice(NoticeLevel::Warning, BLOCKED),
        ],
    );
    let kept = notices(&publisher);
    let [kinded, plain] = kept.as_array().unwrap().as_slice() else {
        panic!("{kept}");
    };
    assert_eq!(kinded["kind"], json!("stopHookLimit"));
    assert_eq!(
        kinded["params"],
        json!({"count": "9", "variable": "ZCODE_STOP_HOOK_BLOCK_CAP"})
    );
    assert_eq!(kinded["text"], json!(CAP));
    assert!(
        plain.get("kind").is_none() && plain.get("params").is_none(),
        "{plain}"
    );
}
