//! 每次提交的执行配置与会话的两个事实的投影（K-C1-INTENT，protocol-v5.md §18.2）：队列项带它自己的配置；`config` 带
//! `permissionGrant` 与 `planTransition`，活会话（事件）与视图建出的同值。

use rust_cli_kernel_api::input_extras::TurnConfig;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::session_api::PlanSwitch;

use super::*;

/// 队列项带着它自己的配置（K-Q2）：没给的项不写。
#[test]
fn a_queue_item_carries_its_own_config() {
    let mut item = queued("c2");
    item.extras.config = TurnConfig {
        selection: Some(Box::new(ModelSelection {
            reasoning: Some("high".to_owned()),
            ..ModelSelection::new("p", "m")
        })),
        mode: Some(PermissionMode::Guarded),
        plan: Some(true),
    };
    let mut plan_only = queued("c3");
    plan_only.extras.config.plan = Some(false);
    let (_, publisher) = live(vec![KernelEvent::QueueChanged {
        items: vec![item, plan_only, queued("c4")],
    }]);
    let queue = snapshot(&publisher)["collections"]["queue"].clone();
    assert_eq!(
        queue[0]["modelSelection"],
        json!({"providerId": "p", "modelId": "m", "options": {"reasoningLevel": "high"}})
    );
    assert_eq!(queue[0]["mode"], "guarded");
    assert_eq!(queue[0]["planEnabled"], true);
    assert_eq!(queue[1]["planEnabled"], false);
    assert!(queue[1].get("mode").is_none() && queue[1].get("modelSelection").is_none());
    assert!(queue[2].get("planEnabled").is_none());
}

/// 「完全访问」与工具切的 plan 进 `config`；用户切的 plan 不动 `planTransition`。视图建出的与事件推出的同值。
#[test]
fn the_grant_and_the_tool_plan_switch_are_in_the_config() {
    let (projection, publisher) = live(vec![
        KernelEvent::PlanChanged {
            enabled: true,
            by: Some(ToolUseId::new("toolu_enter")),
        },
        KernelEvent::ModeChanged {
            mode: PermissionMode::BypassPermissions,
        },
        KernelEvent::ModeGranted {
            tool_use: ToolUseId::new("toolu_1"),
        },
        KernelEvent::PlanChanged {
            enabled: false,
            by: None,
        },
    ]);
    let config = snapshot(&publisher)["state"]["config"].clone();
    assert_eq!(
        config["permissionGrant"],
        json!({"interactionId": "toolu_1"})
    );
    assert_eq!(
        config["planTransition"],
        json!({"toolCallId": "toolu_enter", "planEnabled": true})
    );
    assert_eq!(config["planEnabled"], false);
    assert_eq!(config["mode"], "yolo");
    let cold = SessionView {
        mode_grant: Some(ToolUseId::new("toolu_1")),
        plan_switch: Some(PlanSwitch {
            tool_use: ToolUseId::new("toolu_enter"),
            enabled: true,
        }),
        mode: PermissionMode::BypassPermissions,
        ..view(Vec::new(), Vec::new())
    };
    let (_, content) = Projection::build(&cold, origin(true));
    assert_eq!(content.state["config"], config);
    assert_eq!(projection.mode(), PermissionMode::BypassPermissions);
}

/// 没有授权、工具也没切过 plan：两个键都不写（读端按「没有」，输入框不跟）。
#[test]
fn a_session_without_the_facts_has_no_keys() {
    let (_, content) = Projection::build(&view(Vec::new(), Vec::new()), origin(false));
    let config = content.state["config"].as_object().unwrap().clone();
    assert!(!config.contains_key("permissionGrant") && !config.contains_key("planTransition"));
}

/// 会话的跟随方式（K-S4，protocol-v5.md §22.1）：`config.followupMode` 是内核的值；回合在跑、跟随方式是插话时
/// `admission.input.mode = guide`。视图建出的与事件推出的同值。
#[test]
fn the_follow_up_mode_is_the_kernel_value() {
    let started = KernelEvent::TurnStarted {
        turn: TurnId::new(TURN),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 100,
        works: Vec::new(),
    };
    let (_, publisher) = live(vec![started.clone()]);
    let state = snapshot(&publisher)["state"].clone();
    assert_eq!(state["config"]["followupMode"], "queue");
    assert_eq!(state["admission"]["input"]["mode"], "enqueue");
    let (_, publisher) = live(vec![
        started,
        KernelEvent::FollowUpChanged {
            delivery: Delivery::Steer,
        },
    ]);
    let state = snapshot(&publisher)["state"].clone();
    assert_eq!(state["config"]["followupMode"], "guide");
    assert_eq!(state["admission"]["input"]["mode"], "guide");
    let warm = SessionView {
        follow_up: Delivery::Steer,
        ..view(Vec::new(), Vec::new())
    };
    let (_, content) = Projection::build(&warm, origin(true));
    assert_eq!(content.state["config"]["followupMode"], "guide");
    // 空闲时跟随方式不影响「现在开」
    assert_eq!(content.state["admission"]["input"]["mode"], "startNow");
}

/// 队列项的投递方式（§22.1）：插话项 `intent.delivery = guide`（界面把它画成等这一轮并入的引导，不画成下一轮的排队）；排队项是 queue。
#[test]
fn a_queue_item_says_how_it_is_delivered() {
    let mut steer = queued("c2");
    steer.delivery = Delivery::Steer;
    let (_, publisher) = live(vec![KernelEvent::QueueChanged {
        items: vec![steer, queued("c3")],
    }]);
    let queue = snapshot(&publisher)["collections"]["queue"].clone();
    assert_eq!(
        queue[0]["intent"],
        json!({"delivery": {"requested": "guide", "admitted": "guide"}})
    );
    assert_eq!(
        queue[1]["intent"],
        json!({"delivery": {"requested": "queue", "admitted": "queue"}})
    );
}

/// `config` 过契约 `SessionStateSchema.config` 的严格变体（rust 是生产者，严出）：多写的键不行。
/// A3-DIGEST（22 §15.3）删了 `thoughtLevels`（以前总写 `[]`、界面不读），这里锁住它不再写出。
#[test]
fn the_config_matches_the_strict_contract() {
    let file: serde_json::Value =
        serde_json::from_str(include_str!("../../schema/session@1.json")).unwrap();
    let schema = rust_cli_protocol_core::schema::Node::compile(&crate::strict::strict_variant(
        &file["schemas"]["SessionStateSchema"]["properties"]["config"],
    ))
    .unwrap();
    let (_, content) = Projection::build(&view(Vec::new(), Vec::new()), origin(false));
    schema
        .validate(&content.state["config"], "")
        .unwrap_or_else(|error| panic!("{error}: {}", content.state["config"]));
}
