//! 输入调度的投影（inbox.rs）：一条输入只存一份，排队的输入与 next 通道都按到达顺序。

use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::session_api::{Delivery, QueuedInput};

use super::*;
use crate::peers::PeerItem;

fn input(command: &str, text: &str, delivery: Delivery) -> QueuedInput {
    QueuedInput::new(CommandId::new(command), text.to_owned(), delivery)
}

fn event(text: &str) -> Waiting {
    Waiting::MonitorEvent(text.to_owned())
}

fn receipt(key: &str) -> Waiting {
    Waiting::Peer(PeerItem::Receipt {
        status: rust_cli_kernel_api::peers::ReceiptStatus::Held,
        msg_ids: vec![key.to_owned()],
        text: key.to_owned(),
        passive: true,
    })
}

fn commands(inbox: &Inbox) -> Vec<String> {
    inbox
        .queued()
        .iter()
        .map(|input| input.command.as_str().to_owned())
        .collect()
}

fn lane(inbox: &Inbox) -> Vec<String> {
    inbox
        .lane()
        .map(|item| match item {
            LaneItem::Input(queued) => queued.input.command.as_str().to_owned(),
            LaneItem::Notice(notice) => notice.waiting.text(),
        })
        .collect()
}

#[test]
fn inputs_and_the_lane_are_projections_in_arrival_order() {
    let mut inbox = Inbox::default();
    inbox.push_input(input("q1", "later", Delivery::Queue), InputKind::Text);
    inbox.push_notice(event("n1"));
    inbox.push_input(input("s1", "steer", Delivery::Steer), InputKind::Text);
    inbox.push_first(input("start", "first", Delivery::Queue));
    assert_eq!(commands(&inbox), ["start", "q1", "s1"]);
    assert_eq!(lane(&inbox), ["start", "n1", "s1"]);
    // 去掉一条输入，两个投影里一起没了。
    assert!(inbox.remove_input(&CommandId::new("s1")).is_some());
    assert_eq!(commands(&inbox), ["start", "q1"]);
    assert_eq!(lane(&inbox), ["start", "n1"]);
    // SessionStart 的首条输入在通道里。
    assert!(inbox.has_lane_input());
}

#[test]
fn a_batch_takes_steered_inputs_first_then_later_ones_and_stops_at_a_notice() {
    let mut inbox = Inbox::default();
    inbox.push_input(input("q1", "later one", Delivery::Queue), InputKind::Text);
    inbox.push_input(input("s1", "steer one", Delivery::Steer), InputKind::Text);
    inbox.push_notice(event("n1"));
    inbox.push_input(input("q2", "later two", Delivery::Queue), InputKind::Text);
    let first = inbox.take_batched(&TurnConfig::default()).unwrap();
    assert_eq!(first.input.command.as_str(), "s1");
    // 通道头是通知：不成批。
    assert!(inbox.take_batched(&TurnConfig::default()).is_none());
    assert!(matches!(inbox.pop_lane_front(), Some(Item::Notice(_))));
    // 通道空了：later 输入按顺序。
    assert_eq!(
        inbox
            .take_batched(&TurnConfig::default())
            .unwrap()
            .input
            .command
            .as_str(),
        "q1"
    );
    assert_eq!(
        inbox
            .take_batched(&TurnConfig::default())
            .unwrap()
            .input
            .command
            .as_str(),
        "q2"
    );
    assert!(inbox.take_batched(&TurnConfig::default()).is_none());
}

#[test]
fn a_batch_stops_at_slash_commands_mentions_and_bash() {
    for (text, kind) in [
        ("/compact", InputKind::Text),
        ("look at @a.txt", InputKind::Text),
        ("!ls", InputKind::Bash("ls".to_owned())),
    ] {
        let mut inbox = Inbox::default();
        inbox.push_input(input("q1", text, Delivery::Queue), kind);
        assert!(
            inbox.take_batched(&TurnConfig::default()).is_none(),
            "{text}"
        );
        assert!(inbox.has_inputs());
    }
}

/// 带正文之外事实的输入不成批（session-lifecycle.md §10.14）：它自己开一轮。
#[test]
fn a_batch_stops_at_an_input_with_extras() {
    let mut inbox = Inbox::default();
    let mut with_file = input("q1", "see this", Delivery::Queue);
    with_file
        .extras
        .files
        .push(rust_cli_kernel_api::input_extras::AttachedFile {
            path: "/Users/dev/a.md".to_owned(),
            name: "a.md".to_owned(),
            media_type: "text/markdown".to_owned(),
            size: 1,
        });
    inbox.push_input(with_file, InputKind::Text);
    assert!(inbox.take_batched(&TurnConfig::default()).is_none());
    assert!(inbox.has_inputs());
}

#[test]
fn passive_notices_at_the_head_are_set_aside_and_put_back_in_order() {
    let mut inbox = Inbox::default();
    inbox.push_notice(receipt("r1"));
    inbox.push_notice(receipt("r2"));
    inbox.push_input(input("s1", "steer", Delivery::Steer), InputKind::Text);
    let passive = inbox.take_passive_prefix();
    assert_eq!(passive.len(), 2);
    assert_eq!(lane(&inbox), ["s1"]);
    inbox.restore_front(passive);
    assert_eq!(lane(&inbox), ["r1", "r2", "s1"]);
}

#[test]
fn a_held_input_keeps_its_hook_result() {
    let mut inbox = Inbox::default();
    inbox.push_input(input("s1", "steer", Delivery::Steer), InputKind::Text);
    inbox.hold(&CommandId::new("s1"), None);
    let Some(LaneItem::Input(queued)) = inbox.lane_front() else {
        panic!("the held input stays in the lane");
    };
    assert!(matches!(queued.screening, Screening::Held(None)));
}

/// 交给主循环的通知按 attachment 的消息 ID 取走（只在内存的身份），不按文字：同样文字的两条只取走交出去的那一条。
#[test]
fn an_absorbed_notice_is_taken_by_its_attachment_id() {
    let mut inbox = Inbox::default();
    inbox.push_notice(event("same line"));
    inbox.push_notice(event("same line"));
    let keys: Vec<NoticeKey> = inbox
        .lane()
        .map(|item| match item {
            LaneItem::Notice(notice) => notice.key,
            LaneItem::Input(_) => panic!("only notices"),
        })
        .collect();
    assert_ne!(keys[0], keys[1]);
    inbox.absorbing(keys[1], MessageId::new("msg_9"));
    assert!(inbox.take_absorbed(&MessageId::new("msg_1")).is_none());
    assert!(inbox.take_absorbed(&MessageId::new("msg_9")).is_some());
    let left: Vec<NoticeKey> = inbox
        .lane()
        .map(|item| match item {
            LaneItem::Notice(notice) => notice.key,
            LaneItem::Input(_) => panic!("only notices"),
        })
        .collect();
    assert_eq!(left, [keys[0]]);
}

/// 改正文之后，插话在工具边界跑过的 UserPromptSubmit 结果作废（session-lifecycle.md §10.21）：那是对旧正文的结论。
#[test]
fn editing_drops_the_screening_of_the_old_text() {
    let mut inbox = Inbox::default();
    inbox.push_input(input("s1", "old", Delivery::Steer), InputKind::Text);
    inbox.hold(
        &CommandId::new("s1"),
        Some(Box::new(rust_cli_kernel_api::hooks::HookOutcome::default())),
    );
    inbox.edit(
        &CommandId::new("s1"),
        &rust_cli_kernel_api::session_api::QueuedEdit::text("new"),
    );
    let queued = inbox.remove_input(&CommandId::new("s1")).unwrap();
    assert_eq!(queued.input.text, "new");
    assert!(queued.screened().is_none());
}

/// 「完全访问」只换带执行配置的输入的模式（§10.25）；排队的压缩只带模型，不消费模式，不改（§10.30）。
#[test]
fn full_access_leaves_a_queued_compaction_alone() {
    use rust_cli_kernel_api::input_extras::{InputExtras, TurnConfig};
    use rust_cli_kernel_api::model::ModelSelection;
    let mut inbox = Inbox::default();
    let config = TurnConfig {
        selection: Some(Box::new(ModelSelection::new(
            "p".to_owned(),
            "m".to_owned(),
        ))),
        ..TurnConfig::default()
    };
    let mut configured = input("q1", "q1", Delivery::Queue);
    configured.extras = InputExtras {
        config: TurnConfig {
            mode: Some(PermissionMode::Default),
            ..config.clone()
        },
        ..InputExtras::default()
    };
    inbox.push_input(configured, InputKind::Text);
    inbox.push_compaction(CommandId::new("k1"), config.clone());
    assert!(inbox.grant_mode(PermissionMode::BypassPermissions));
    let queued = inbox.queued();
    assert_eq!(
        queued[0].extras.config.mode,
        Some(PermissionMode::BypassPermissions)
    );
    assert_eq!(queued[1].extras.config, config);
}

/// 重排只动这一项在序列里的位置（session-lifecycle.md §10.21）；挪到最后排在全部项之后。
#[test]
fn moving_an_input_keeps_the_others_in_order() {
    let mut inbox = Inbox::default();
    for id in ["q1", "q2", "q3"] {
        inbox.push_input(input(id, id, Delivery::Queue), InputKind::Text);
    }
    inbox.move_before(&CommandId::new("q3"), Some(&CommandId::new("q1")));
    assert_eq!(commands(&inbox), ["q3", "q1", "q2"]);
    inbox.move_before(&CommandId::new("q3"), None);
    assert_eq!(commands(&inbox), ["q1", "q2", "q3"]);
    // 前端看到的这一项是整项，没有位置（§10.30）：挪动别的项不改它
    assert_eq!(
        inbox.queued_input(&CommandId::new("q2")),
        Some(&input("q2", "q2", Delivery::Queue))
    );
}
