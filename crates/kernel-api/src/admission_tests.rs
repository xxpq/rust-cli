//! 准入的规则表（session-lifecycle.md §10.4）。

use super::*;
use crate::ids::MessageId;
use crate::session_api::RewindScope;

fn view(activity: Activity) -> AdmissionView {
    AdmissionView {
        activity,
        closing: false,
        held: false,
        frozen: false,
        compaction: false,
        follow_up: Delivery::Queue,
    }
}

fn submit(delivery: Delivery) -> Command {
    Command::Submit {
        text: "hi".to_owned(),
        extras: Default::default(),
        delivery,
        target: None,
    }
}

fn turn() -> Activity {
    Activity::Turn(TurnId::new("turn_1"))
}

#[test]
fn inputs_start_when_idle_and_queue_while_busy() {
    let idle = view(Activity::Idle);
    assert_eq!(admit(&idle, &submit(Delivery::Steer)), Admission::Run);
    for busy in [turn(), Activity::Job { cancellable: true }] {
        let busy = view(busy);
        assert_eq!(
            admit(&busy, &submit(Delivery::Steer)),
            Admission::Enqueue(Delivery::Steer)
        );
        assert_eq!(
            admit(&busy, &submit(Delivery::Queue)),
            Admission::Enqueue(Delivery::Queue)
        );
        let content = Command::SubmitContent {
            content: Vec::new(),
            delivery: Delivery::Steer,
        };
        assert_eq!(admit(&busy, &content), Admission::Enqueue(Delivery::Queue));
    }
}

#[test]
fn interrupt_needs_something_that_can_stop() {
    assert_eq!(
        admit(&view(turn()), &Command::Interrupt { target: None }),
        Admission::Run
    );
    assert_eq!(
        admit(
            &view(Activity::Job { cancellable: true }),
            &Command::Interrupt { target: None }
        ),
        Admission::Run
    );
    for idle in [Activity::Idle, Activity::Job { cancellable: false }] {
        assert_eq!(
            admit(&view(idle), &Command::Interrupt { target: None }),
            Admission::Deny(Refusal::NotRunning)
        );
    }
}

#[test]
fn idle_only_commands_are_busy_while_anything_runs() {
    let commands = [
        Command::Compact { instructions: None },
        Command::Fork { at: None },
        Command::Rewind {
            target: MessageId::new("m"),
            scope: RewindScope::Conversation,
        },
        Command::RestoreCancelled,
    ];
    for command in &commands {
        assert_eq!(admit(&view(Activity::Idle), command), Admission::Run);
        assert_eq!(
            admit(&view(turn()), command),
            Admission::Deny(Refusal::Busy)
        );
    }
    // 其余命令（改模型、改模式……）在跑的时候也接受。
    assert_eq!(
        admit(&view(turn()), &Command::SetPlan { enabled: true }),
        Admission::Run
    );
}

#[test]
fn a_closing_session_takes_nothing() {
    let closing = AdmissionView {
        closing: true,
        ..view(Activity::Idle)
    };
    assert_eq!(
        admit(&closing, &Command::SetPlan { enabled: true }),
        Admission::Deny(Refusal::Closed)
    );
}

/// 指名的停止与插话（session-lifecycle.md §10.5）。
#[test]
fn named_commands_act_only_on_their_turn() {
    let t1 = TurnId::new("turn_1");
    let t2 = TurnId::new("turn_2");
    let stop = Command::Interrupt {
        target: Some(t1.clone()),
    };
    assert_eq!(
        admit(&view(Activity::Turn(t1.clone())), &stop),
        Admission::Run
    );
    assert_eq!(
        admit(&view(Activity::Turn(t2.clone())), &stop),
        Admission::Deny(Refusal::StopTargetChanged {
            current: t2.clone()
        })
    );
    assert_eq!(admit(&view(Activity::Idle), &stop), Admission::Unchanged);
    let guide = Command::Submit {
        text: "hi".to_owned(),
        extras: Default::default(),
        delivery: Delivery::Steer,
        target: Some(t1.clone()),
    };
    assert_eq!(
        admit(&view(Activity::Turn(t1)), &guide),
        Admission::Enqueue(Delivery::Steer)
    );
    assert_eq!(
        admit(&view(Activity::Turn(t2)), &guide),
        Admission::Enqueue(Delivery::Queue)
    );
    assert_eq!(admit(&view(Activity::Idle), &guide), Admission::Run);
}

/// 扣住的队列（session-lifecycle.md §10.6）：空闲时输入要先等用户决定；运行中照常排队。
#[test]
fn a_held_queue_asks_for_a_choice_before_new_input() {
    let held = |activity| AdmissionView {
        held: true,
        ..view(activity)
    };
    assert_eq!(
        admit(&held(Activity::Idle), &submit(Delivery::Queue)),
        Admission::Deny(Refusal::QueueChoiceRequired)
    );
    assert_eq!(
        admit(&held(turn()), &submit(Delivery::Queue)),
        Admission::Enqueue(Delivery::Queue)
    );
    // 指名回合已经结束的插话按普通输入走，同样要先决定（02 §11.2 D 补 3）。
    let guide = Command::Submit {
        text: "hi".to_owned(),
        extras: Default::default(),
        delivery: Delivery::Steer,
        target: Some(TurnId::new("turn_1")),
    };
    assert_eq!(
        admit(&held(Activity::Idle), &guide),
        Admission::Deny(Refusal::QueueChoiceRequired)
    );
    // 放开与取回不受影响。
    assert_eq!(
        admit(
            &held(Activity::Idle),
            &Command::SetAutoDrain { enabled: true }
        ),
        Admission::Run
    );
}

/// `SubmitKeepingQueue`（§10.13）：扣住时也开；在跑时 Busy。
#[test]
fn submit_keeping_queue_runs_only_when_idle_even_if_held() {
    let keep = Command::SubmitKeepingQueue {
        text: "now".to_owned(),
        extras: Default::default(),
        expected_held: None,
    };
    let held = AdmissionView {
        held: true,
        ..view(Activity::Idle)
    };
    assert_eq!(admit(&held, &keep), Admission::Run);
    assert_eq!(admit(&view(turn()), &keep), Admission::Deny(Refusal::Busy));
}

/// `Promote`（§10.13）：同指名停止，只打断指名的回合；没指名时有回合在跑就拒绝。
#[test]
fn promote_follows_the_named_turn_table() {
    let promote = |target: Option<&str>| Command::Promote {
        command: crate::ids::CommandId::new("c2"),
        target: target.map(TurnId::new),
    };
    let changed = Admission::Deny(Refusal::StopTargetChanged {
        current: TurnId::new("turn_1"),
    });
    assert_eq!(admit(&view(Activity::Idle), &promote(None)), Admission::Run);
    assert_eq!(
        admit(&view(turn()), &promote(Some("turn_1"))),
        Admission::Run
    );
    assert_eq!(admit(&view(turn()), &promote(Some("turn_0"))), changed);
    assert_eq!(admit(&view(turn()), &promote(None)), changed);
}

/// 带正文之外事实的输入（session-lifecycle.md §10.14）运行中一律排队：指名当前回合的插话也不并入。
#[test]
fn inputs_with_extras_queue_instead_of_steering() {
    let extras = crate::input_extras::InputExtras {
        files: vec![crate::input_extras::AttachedFile {
            path: "/Users/dev/a.md".to_owned(),
            name: "a.md".to_owned(),
            media_type: "text/markdown".to_owned(),
            size: 1,
        }],
        ..crate::input_extras::InputExtras::default()
    };
    let with = |target: Option<TurnId>| Command::Submit {
        text: "see".to_owned(),
        extras: extras.clone(),
        delivery: Delivery::Steer,
        target,
    };
    let busy = view(turn());
    assert_eq!(
        admit(&busy, &with(None)),
        Admission::Enqueue(Delivery::Queue)
    );
    assert_eq!(
        admit(&busy, &with(Some(TurnId::new("turn_1")))),
        Admission::Enqueue(Delivery::Queue)
    );
    assert_eq!(admit(&view(Activity::Idle), &with(None)), Admission::Run);
}

/// 只带执行配置的输入（§10.25，App 的每条输入都带）照常插话：配置不是挂在开回合的输入上的材料。
#[test]
fn a_turn_config_alone_still_steers() {
    let extras = crate::input_extras::InputExtras {
        config: crate::input_extras::TurnConfig {
            plan: Some(true),
            ..crate::input_extras::TurnConfig::default()
        },
        ..crate::input_extras::InputExtras::default()
    };
    let with = |target: Option<TurnId>| Command::Submit {
        text: "also".to_owned(),
        extras: extras.clone(),
        delivery: Delivery::Steer,
        target,
    };
    let busy = view(turn());
    assert_eq!(
        admit(&busy, &with(None)),
        Admission::Enqueue(Delivery::Steer)
    );
    assert_eq!(
        admit(&busy, &with(Some(TurnId::new("turn_1")))),
        Admission::Enqueue(Delivery::Steer)
    );
}

/// 「取空扣住的队列再发」（§10.16）就是对扣住的处置：扣住时也接受；在跑时同普通输入排队（不插话）。
#[test]
fn submit_discarding_queue_is_the_choice_for_a_held_queue() {
    let discard = Command::SubmitDiscardingQueue {
        text: "now".to_owned(),
        extras: Default::default(),
        expected_held: None,
    };
    let held = AdmissionView {
        held: true,
        ..view(Activity::Idle)
    };
    assert_eq!(admit(&held, &discard), Admission::Run);
    assert_eq!(
        admit(&view(turn()), &discard),
        Admission::Enqueue(Delivery::Queue)
    );
}

/// v5 的压缩（session-lifecycle.md §10.23，02 §4.2、R21）：空闲时现在压；忙或队列扣住时排队；已有在跑或排队的压缩时拒绝；
/// TUI 的 `Compact` 不变（忙时 Busy）。
#[test]
fn compact_or_queue_queues_behind_work_and_locks_a_second_compaction() {
    let compact = Command::CompactOrQueue { selection: None };
    assert_eq!(admit(&view(Activity::Idle), &compact), Admission::Run);
    assert_eq!(
        admit(&view(turn()), &compact),
        Admission::Enqueue(Delivery::Queue)
    );
    assert_eq!(
        admit(&view(Activity::Job { cancellable: true }), &compact),
        Admission::Enqueue(Delivery::Queue)
    );
    let held = AdmissionView {
        held: true,
        ..view(Activity::Idle)
    };
    assert_eq!(admit(&held, &compact), Admission::Enqueue(Delivery::Queue));
    let locked = |activity| AdmissionView {
        compaction: true,
        ..view(activity)
    };
    assert_eq!(
        admit(&locked(Activity::Idle), &compact),
        Admission::Deny(Refusal::CompactLocked)
    );
    assert_eq!(
        admit(&locked(turn()), &compact),
        Admission::Deny(Refusal::CompactLocked)
    );
    let closing = AdmissionView {
        closing: true,
        ..locked(Activity::Idle)
    };
    assert_eq!(admit(&closing, &compact), Admission::Deny(Refusal::Closed));
    assert_eq!(
        admit(&view(turn()), &Command::Compact { instructions: None }),
        Admission::Deny(Refusal::Busy)
    );
}

/// 改与挪排队的项、编辑锁、「放行并切模式」的回答（§10.21、§10.22、§10.59）：任何时候都接受（判据在 actor 里），只有关闭时拒绝。
#[test]
fn queue_edits_and_full_access_run_in_any_activity() {
    let c2 = || crate::ids::CommandId::new("c2");
    let seen = crate::session_api::QueuedInput::new(c2(), "a", Delivery::Queue);
    let commands = [
        Command::EditQueued {
            command: c2(),
            edit: Box::new(crate::session_api::QueuedEdit::text("b")),
            guard: crate::session_api::EditGuard::Seen {
                expected: Box::new(seen),
            },
        },
        Command::MoveQueued {
            command: c2(),
            before: None,
        },
        Command::LockQueued {
            command: c2(),
            editor: "e1".to_owned(),
        },
        Command::UnlockQueued { command: c2() },
        Command::ApplyQueuedConfig {
            commands: vec![c2()],
            config: Default::default(),
        },
        Command::ResolveSwitchingMode {
            interaction: crate::ids::InteractionId::new("interaction_1"),
            mode: crate::permission::PermissionMode::Guarded,
        },
    ];
    let held = AdmissionView {
        held: true,
        ..view(Activity::Idle)
    };
    for command in &commands {
        assert_eq!(admit(&view(Activity::Idle), command), Admission::Run);
        assert_eq!(admit(&view(turn()), command), Admission::Run);
        assert_eq!(admit(&held, command), Admission::Run);
    }
}

fn follow_up() -> Command {
    Command::SubmitFollowUp {
        text: "hi".to_owned(),
        extras: Default::default(),
    }
}

/// 按会话的跟随方式投递的输入（session-lifecycle.md §10.27，K-S4）：回合在跑时 `Steer` 插话、`Queue` 排队；空闲时同普通输入；
/// 后台工作在跑、自己一轮的输入一律排队。
#[test]
fn a_follow_up_input_is_delivered_the_session_way() {
    let steering = |activity| AdmissionView {
        follow_up: Delivery::Steer,
        ..view(activity)
    };
    assert_eq!(
        admit(&steering(turn()), &follow_up()),
        Admission::Enqueue(Delivery::Steer)
    );
    assert_eq!(
        admit(&view(turn()), &follow_up()),
        Admission::Enqueue(Delivery::Queue)
    );
    assert_eq!(
        admit(&steering(Activity::Idle), &follow_up()),
        Admission::Run
    );
    assert_eq!(
        admit(&steering(Activity::Job { cancellable: true }), &follow_up()),
        Admission::Enqueue(Delivery::Queue)
    );
    let held = AdmissionView {
        held: true,
        ..steering(Activity::Idle)
    };
    assert_eq!(
        admit(&held, &follow_up()),
        Admission::Deny(Refusal::QueueChoiceRequired)
    );
    let own_turn = Command::SubmitFollowUp {
        text: "hi".to_owned(),
        extras: crate::input_extras::InputExtras {
            disallowed_tools: vec!["Bash".to_owned()],
            ..Default::default()
        },
    };
    assert_eq!(
        admit(&steering(turn()), &own_turn),
        Admission::Enqueue(Delivery::Queue)
    );
}

/// 「现在发」（session-lifecycle.md §10.27，13 Q17）：回合在跑时现在执行（由会话 actor 抢占）；空闲时同普通输入；后台工作在跑时排队。
#[test]
fn a_start_now_input_preempts_a_running_turn() {
    let now = Command::SubmitNow {
        text: "hi".to_owned(),
        extras: Default::default(),
    };
    assert_eq!(admit(&view(turn()), &now), Admission::Run);
    assert_eq!(admit(&view(Activity::Idle), &now), Admission::Run);
    assert_eq!(
        admit(&view(Activity::Job { cancellable: true }), &now),
        Admission::Enqueue(Delivery::Queue)
    );
    let held = AdmissionView {
        held: true,
        ..view(Activity::Idle)
    };
    assert_eq!(
        admit(&held, &now),
        Admission::Deny(Refusal::QueueChoiceRequired)
    );
    // 扣住着、回合在跑：照样抢占（扣住只拦空闲时开下一条）
    let held_running = AdmissionView {
        held: true,
        ..view(turn())
    };
    assert_eq!(admit(&held_running, &now), Admission::Run);
}

/// 改跟随方式是设值：任何时候都现在执行（关闭中除外）。
#[test]
fn setting_the_follow_up_mode_always_runs() {
    let set = Command::SetFollowUp {
        delivery: Delivery::Steer,
    };
    for activity in [Activity::Idle, turn(), Activity::Job { cancellable: false }] {
        assert_eq!(admit(&view(activity), &set), Admission::Run);
    }
}

fn replace() -> Command {
    Command::ReplaceInput {
        target: MessageId::new("msg_u1"),
        text: "again".to_owned(),
        extras: Default::default(),
        restore_code: false,
        stop_cut_works: false,
    }
}

/// 换掉一条输入（session-lifecycle.md §10.53）：回合在跑时抢占它；压缩与会话的后台工作在跑时不换；扣住的队列不挡它。
#[test]
fn replacing_an_input_preempts_a_turn_but_not_a_compaction_or_a_job() {
    assert_eq!(admit(&view(Activity::Idle), &replace()), Admission::Run);
    assert_eq!(admit(&view(turn()), &replace()), Admission::Run);
    let compacting = AdmissionView {
        compaction: true,
        ..view(turn())
    };
    assert_eq!(
        admit(&compacting, &replace()),
        Admission::Deny(Refusal::Busy)
    );
    // 空闲、扣住的队列里排着压缩：不挡（压缩还没开）
    let held = AdmissionView {
        held: true,
        compaction: true,
        ..view(Activity::Idle)
    };
    assert_eq!(admit(&held, &replace()), Admission::Run);
    for job in [true, false] {
        assert_eq!(
            admit(&view(Activity::Job { cancellable: job }), &replace()),
            Admission::Deny(Refusal::Busy)
        );
    }
    let closing = AdmissionView {
        closing: true,
        ..view(Activity::Idle)
    };
    assert_eq!(
        admit(&closing, &replace()),
        Admission::Deny(Refusal::Closed)
    );
}

fn fork_as() -> Command {
    Command::ForkFromInputAs {
        input: MessageId::new("msg_u1"),
        session: crate::ids::SessionId::new("ses_child"),
    }
}

/// v5 的 fork（session-lifecycle.md §10.57）：回合在跑时也分（不打断它）；压缩与会话的后台工作在跑时不分。TUI 的 `/fork` 照旧只在空闲时。
#[test]
fn fork_as_runs_beside_a_turn_but_not_beside_a_compaction() {
    assert_eq!(admit(&view(Activity::Idle), &fork_as()), Admission::Run);
    assert_eq!(admit(&view(turn()), &fork_as()), Admission::Run);
    let compacting = AdmissionView {
        compaction: true,
        ..view(turn())
    };
    assert_eq!(
        admit(&compacting, &fork_as()),
        Admission::Deny(Refusal::Busy)
    );
    for job in [true, false] {
        assert_eq!(
            admit(&view(Activity::Job { cancellable: job }), &fork_as()),
            Admission::Deny(Refusal::Busy)
        );
    }
    let closing = AdmissionView {
        closing: true,
        ..view(Activity::Idle)
    };
    assert_eq!(
        admit(&closing, &fork_as()),
        Admission::Deny(Refusal::Closed)
    );
    let tui = Command::ForkFromInput {
        input: MessageId::new("msg_u1"),
    };
    assert_eq!(admit(&view(turn()), &tui), Admission::Deny(Refusal::Busy));
}

/// v5 的侧聊（session-lifecycle.md §10.58）：只读已提交的记录、不改父会话，所以回合、压缩、会话的后台工作在跑时都执行；只有关闭中拒绝。
#[test]
fn side_chat_as_runs_whatever_the_session_does() {
    let side_chat = Command::SideChatAs {
        session: crate::ids::SessionId::new("sess_side"),
    };
    let compacting = AdmissionView {
        compaction: true,
        ..view(turn())
    };
    let held = AdmissionView {
        held: true,
        ..view(Activity::Idle)
    };
    for busy in [
        view(Activity::Idle),
        view(turn()),
        compacting,
        held,
        view(Activity::Job { cancellable: true }),
        view(Activity::Job { cancellable: false }),
    ] {
        assert_eq!(admit(&busy, &side_chat), Admission::Run, "{busy:?}");
    }
    let closing = AdmissionView {
        closing: true,
        ..view(Activity::Idle)
    };
    assert_eq!(
        admit(&closing, &side_chat),
        Admission::Deny(Refusal::Closed)
    );
}

/// 队首被编辑器锁着（session-lifecycle.md §10.59，照 v4 的 computeInputRouting）：空闲、没有扣住时输入与压缩排到队尾，不越过冻结的项；
/// 扣住优先（照旧要选择）；在跑时照常（插话照常插话）。别的项照常能提升。
#[test]
fn frozen_head_queues_idle_inputs_behind_it() {
    let frozen = |activity| AdmissionView {
        frozen: true,
        ..view(activity)
    };
    let idle = frozen(Activity::Idle);
    for command in [
        submit(Delivery::Queue),
        submit(Delivery::Steer),
        Command::SubmitFollowUp {
            text: "x".into(),
            extras: Default::default(),
        },
        Command::SubmitNow {
            text: "x".into(),
            extras: Default::default(),
        },
        Command::Bash {
            command: "ls".into(),
            delivery: Delivery::Queue,
        },
        Command::CompactOrQueue { selection: None },
    ] {
        assert_eq!(
            admit(&idle, &command),
            Admission::Enqueue(Delivery::Queue),
            "{command:?}"
        );
    }
    let held = AdmissionView {
        held: true,
        ..idle.clone()
    };
    assert_eq!(
        admit(&held, &submit(Delivery::Queue)),
        Admission::Deny(Refusal::QueueChoiceRequired)
    );
    let turn = TurnId::new("t1");
    let running = frozen(Activity::Turn(turn.clone()));
    assert_eq!(
        admit(
            &running,
            &Command::Submit {
                text: "x".into(),
                extras: Default::default(),
                delivery: Delivery::Steer,
                target: Some(turn),
            }
        ),
        Admission::Enqueue(Delivery::Steer)
    );
    assert_eq!(
        admit(
            &idle,
            &Command::Promote {
                command: CommandId::new("q2"),
                target: None,
            }
        ),
        Admission::Run
    );
}
