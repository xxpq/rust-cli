//! 每次提交的执行配置、排队项的配置、授权与 plan 切换的事实（docs/specs/rust-cli/session-lifecycle.md §10.25）。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use rust_cli_kernel_api::input_extras::{InputExtras, TurnConfig};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, Delivery, KernelEvent, PlanSwitch, SequencedEvent, SessionHandle,
};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 测试里的工具：`Write` 问一次权限；`EnterPlanMode` 经会话操作打开 plan；`Mode` 把它被调用时的模式作为结果（判决与工具看的是同一个模式，
/// 都经 `SessionOps::view`）；`Read` 直接完成（给插话一个工具边界）。
struct Probe(&'static str);

#[async_trait::async_trait]
impl Tool for Probe {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        self.0
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let failed = |message: String| ToolError::Failed { message };
        match self.0 {
            "Write" => {
                let request = InteractionRequest::Permission {
                    tool_use: context.tool_use.clone(),
                    tool: "Write".to_owned(),
                    input: json!({}),
                    message: "may I?".to_owned(),
                    suggestions: Vec::new(),
                    rule_seed: None,
                    asked_by: None,
                    machine: Default::default(),
                    once: false,
                };
                context
                    .interactions
                    .ask(request)
                    .await
                    .map_err(|error| failed(error.to_string()))?;
            }
            "EnterPlanMode" => context
                .session_ops
                .set_plan(true, &context.tool_use)
                .await
                .map_err(|error| failed(error.to_string()))?,
            _ => {}
        }
        let text = if self.0 == "Mode" {
            format!("mode {:?}", context.mode)
        } else {
            "done".to_owned()
        };
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text { text }],
            is_error: false,
            display: Value::Null,
        })
    }
}

fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Probe("Write")),
        Arc::new(Probe("EnterPlanMode")),
        Arc::new(Probe("Mode")),
        Arc::new(Probe("Read")),
    ]
}

fn config(mode: Option<PermissionMode>, plan: Option<bool>) -> TurnConfig {
    TurnConfig {
        selection: None,
        mode,
        plan,
    }
}

fn input(id: &str, text: &str, delivery: Delivery, config: TurnConfig) -> CommandEnvelope {
    envelope(
        id,
        Command::Submit {
            text: text.to_owned(),
            extras: InputExtras {
                config,
                ..InputExtras::default()
            },
            delivery,
            target: None,
        },
    )
}

type Reply =
    Vec<Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>>;

/// 第一条回复问一次权限：回合停在询问上，期间排进来的输入都在队里。`rest` 是之后的回复。返回问到时的交互 id。
async fn asking(
    rest: Vec<Reply>,
) -> (
    Built,
    Arc<ScriptedModel>,
    BoxStream<'static, SequencedEvent>,
    InteractionId,
) {
    let mut script = vec![tool_use_response("toolu_1", "Write", &json!({}))];
    script.extend(rest);
    let model = Arc::new(ScriptedModel::new(script));
    let mut setup = Setup::new(model.clone());
    setup.tools = tools();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write it")).await;
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::InteractionAsked { .. })
    })
    .await;
    let Some(KernelEvent::InteractionAsked { interaction, .. }) = seen.last() else {
        unreachable!("until stops at InteractionAsked");
    };
    let interaction = interaction.clone();
    (built, model, events, interaction)
}

fn allow(interaction: &InteractionId) -> CommandEnvelope {
    envelope(
        "a1",
        Command::ResolveInteraction {
            interaction: interaction.clone(),
            answer: rust_cli_kernel_api::interaction::Answer::Permission(
                rust_cli_kernel_api::interaction::PermissionAnswer::Allow {
                    remember: Vec::new(),
                },
            ),
        },
    )
}

/// 排队的项带着它自己的配置；轮到它开回合时，配置先写成会话的配置（模型、模式、plan 的事件排在 `TurnStarted` 之前），这一轮按它跑。
/// 落库的是执行状态，不写项目的持久模式（照 v4 的「开跑时套用」）。
#[tokio::test]
async fn a_queued_input_opens_its_turn_with_its_own_config() {
    let (built, model, mut events, interaction) =
        asking(vec![text_response(&["ok"]), text_response(&["ok"])]).await;
    let wanted = TurnConfig {
        selection: Some(Box::new(ModelSelection::new("fixture", "other"))),
        mode: Some(PermissionMode::Guarded),
        plan: Some(true),
    };
    built
        .session
        .send(input("c2", "next", Delivery::Queue, wanted.clone()))
        .await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue[0].extras.config, wanted);
    // 排着的时候会话不变
    assert_eq!(view.mode, PermissionMode::Default);
    assert!(!view.plan);
    built.session.send(allow(&interaction)).await;
    until(&mut events, finished).await;
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let at = |wanted: fn(&KernelEvent) -> bool| seen.iter().position(wanted);
    let started = at(|event| matches!(event, KernelEvent::TurnStarted { .. }));
    for changed in [
        at(
            |event| matches!(event, KernelEvent::ModelChanged { selection } if selection.model == "other"),
        ),
        at(|event| {
            matches!(
                event,
                KernelEvent::ModeChanged {
                    mode: PermissionMode::Guarded
                }
            )
        }),
        at(|event| {
            matches!(
                event,
                KernelEvent::PlanChanged {
                    enabled: true,
                    by: None
                }
            )
        }),
    ] {
        assert!(changed.is_some() && changed < started, "{seen:?}");
    }
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.mode, PermissionMode::Guarded);
    assert!(view.plan);
    assert_eq!(view.selection.model, "other");
    assert_eq!(model.requests().last().unwrap().selection.model, "other");
    let facts = built.store.facts(built.session.id());
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact, Fact::ExecutionChanged { state } if state.plan)),
        "{facts:?}"
    );
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::ProjectModeSet { .. })),
        "{facts:?}"
    );
}

/// 配置不同的两条不成批（成批了后一条的配置就丢了）；配置相同的照常成批。
#[tokio::test]
async fn only_inputs_with_the_same_config_batch() {
    let (built, model, mut events, interaction) = asking(vec![
        text_response(&["ok"]),
        text_response(&["ok"]),
        text_response(&["ok"]),
    ])
    .await;
    let plan = config(None, Some(true));
    for (id, text, config) in [
        ("c2", "two", plan.clone()),
        ("c3", "three", plan),
        ("c4", "four", config(None, Some(false))),
    ] {
        built
            .session
            .send(input(id, text, Delivery::Queue, config))
            .await;
    }
    built.session.send(allow(&interaction)).await;
    for _ in 0..3 {
        until(&mut events, finished).await;
    }
    // 询问的回合两次请求；之后 two + three 一轮、four 一轮
    let requests = model.requests();
    assert_eq!(requests.len(), 4, "{requests:#?}");
    let third = serde_json::to_string(&requests[2].messages).unwrap();
    assert!(third.contains("two") && third.contains("three") && !third.contains("four"));
    assert!(!built.session.view().await.unwrap().plan);
}

/// 配置与会话现在的不同的插话照常在工具边界并入（主会话审定 2026-11-06）：并入的同一步里它的模式写成会话的执行状态，这一轮之后的调用就按它；
/// 它的模型写成会话的选择，这一轮不换（一次运行不换模型），下一轮用它。
#[tokio::test]
async fn a_steer_with_another_config_joins_the_turn_and_switches_the_mode_now() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![
            tool_use_response("toolu_1", "Read", &json!({})),
            tool_use_response("toolu_2", "Mode", &json!({})),
            text_response(&["first"]),
            text_response(&["second"]),
        ])
        .gated_at(0, gate.clone()),
    );
    let mut setup = Setup::new(model.clone());
    setup.tools = tools();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "read it")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let steer = TurnConfig {
        selection: Some(Box::new(ModelSelection::new("fixture", "other"))),
        mode: Some(PermissionMode::Guarded),
        plan: None,
    };
    built
        .session
        .send(input("c2", "be careful", Delivery::Steer, steer))
        .await;
    gate.notify_one();
    until(&mut events, finished).await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3, "{requests:#?}");
    // 插话进了这一轮：工具之后的那次请求里有它
    let after_read = serde_json::to_string(&requests[1].messages).unwrap();
    assert!(after_read.contains("be careful"), "{after_read}");
    // 这一轮之后的调用按插话的模式（`Mode` 的结果在第三次请求里）
    let after_mode = serde_json::to_string(&requests[2].messages).unwrap();
    assert!(after_mode.contains("mode Guarded"), "{after_mode}");
    // 这一轮不换模型；会话的选择换了
    assert!(
        requests
            .iter()
            .all(|request| request.selection.model == "scripted")
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(view.mode, PermissionMode::Guarded);
    assert_eq!(view.selection.model, "other");
    // 下一轮用新模型
    built.session.send(submit("c3", "next")).await;
    until(&mut events, finished).await;
    assert_eq!(model.requests().last().unwrap().selection.model, "other");
}

/// 「完全访问」在同一步里把排队的、带配置的项的模式换成完全访问（plan 不动），没带配置的不动；发 `ModeGranted`，视图带着它。
/// 之后排进来的项带它自己的模式（照 v4：授权时取定）。
#[tokio::test]
async fn full_access_rewrites_the_queued_configs_and_reports_the_grant() {
    // 授权之后这一轮再问一次：回合停住，排着的项不会开（视图里还看得到它们）
    let (built, _model, mut events, interaction) =
        asking(vec![tool_use_response("toolu_2", "Write", &json!({}))]).await;
    for (id, config) in [
        ("c2", config(Some(PermissionMode::Guarded), Some(true))),
        ("c3", config(Some(PermissionMode::Default), Some(false))),
        ("c4", TurnConfig::default()),
    ] {
        built
            .session
            .send(input(id, id, Delivery::Queue, config))
            .await;
    }
    let ack = built
        .session
        .send(envelope(
            "f1",
            Command::ResolveSwitchingMode {
                interaction: interaction.clone(),
                mode: rust_cli_kernel_api::permission::PermissionMode::BypassPermissions,
            },
        ))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::InteractionResolved { .. })
    })
    .await;
    let granted = seen.iter().position(|event| {
        matches!(event, KernelEvent::ModeGranted { tool_use } if tool_use.as_str() == "toolu_1")
    });
    let resolved = seen.len() - 1;
    assert!(granted.is_some_and(|at| at < resolved), "{seen:?}");
    let view = built.session.view().await.unwrap();
    assert_eq!(view.mode_grant, Some(ToolUseId::new("toolu_1")));
    let configs: Vec<TurnConfig> = view
        .queue
        .iter()
        .map(|queued| queued.extras.config.clone())
        .collect();
    let full = Some(PermissionMode::BypassPermissions);
    assert_eq!(
        configs,
        [
            config(full, Some(true)),
            config(full, Some(false)),
            TurnConfig::default()
        ]
    );
    built
        .session
        .send(input(
            "c5",
            "c5",
            Delivery::Queue,
            config(Some(PermissionMode::Guarded), None),
        ))
        .await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        view.queue.last().unwrap().extras.config.mode,
        Some(PermissionMode::Guarded)
    );
}

/// 会话已经是完全访问：模式不变（没有 `ModeChanged`），这次回答照样是授权（`ModeGranted`，同 v4）。
#[tokio::test]
async fn full_access_on_a_full_access_session_still_reports_the_grant() {
    let (built, _model, mut events, interaction) = asking(vec![text_response(&["ok"])]).await;
    let ack = built
        .session
        .send(envelope(
            "m1",
            Command::SetMode {
                mode: PermissionMode::BypassPermissions,
            },
        ))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    until(&mut events, |event| {
        matches!(event, KernelEvent::ModeChanged { .. })
    })
    .await;
    built
        .session
        .send(envelope(
            "f1",
            Command::ResolveSwitchingMode {
                interaction,
                mode: rust_cli_kernel_api::permission::PermissionMode::BypassPermissions,
            },
        ))
        .await;
    let seen = until(&mut events, finished).await;
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::ModeGranted { .. })),
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, KernelEvent::ModeChanged { .. })),
        "{seen:?}"
    );
}

/// 工具切的 plan 带上切它的调用，视图记着最近一次；用户切的 plan 不带、也不改它。
#[tokio::test]
async fn a_tool_plan_switch_names_its_call() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_enter", "EnterPlanMode", &json!({})),
        text_response(&["planned"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = tools();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "plan it")).await;
    let seen = until(&mut events, finished).await;
    assert!(
        seen.iter().any(|event| matches!(
            event,
            KernelEvent::PlanChanged { enabled: true, by: Some(by) } if by.as_str() == "toolu_enter"
        )),
        "{seen:?}"
    );
    let switched = Some(PlanSwitch {
        tool_use: ToolUseId::new("toolu_enter"),
        enabled: true,
    });
    assert_eq!(built.session.view().await.unwrap().plan_switch, switched);
    built
        .session
        .send(envelope("p1", Command::SetPlan { enabled: false }))
        .await;
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::PlanChanged { .. })
    })
    .await;
    assert!(
        matches!(
            seen.last(),
            Some(KernelEvent::PlanChanged {
                enabled: false,
                by: None
            })
        ),
        "{seen:?}"
    );
    let view = built.session.view().await.unwrap();
    assert!(!view.plan);
    assert_eq!(view.plan_switch, switched);
}

/// 输入记录带这一轮用的配置（session-lifecycle.md §10.59）：带了配置的输入记下套用之后会话的模型、模式与 plan（三项齐，没带的项是会话那时的值），
/// 成批进同一轮的后续输入各记一份；没带配置的输入（TUI 与 `-p`）没有记录。活会话的事件与视图同一份。
#[tokio::test]
async fn an_input_records_the_config_its_turn_ran_with() {
    let (built, _model, mut events, interaction) =
        asking(vec![text_response(&["one"]), text_response(&["two"])]).await;
    for (id, text) in [("c2", "second"), ("c3", "third")] {
        built
            .session
            .send(input(
                id,
                text,
                Delivery::Queue,
                config(Some(PermissionMode::AcceptEdits), None),
            ))
            .await;
    }
    built.session.send(submit("c4", "plain")).await;
    built
        .session
        .send(envelope(
            "a1",
            Command::ResolveInteraction {
                interaction,
                answer: rust_cli_kernel_api::interaction::Answer::Permission(
                    rust_cli_kernel_api::interaction::PermissionAnswer::Allow {
                        remember: Vec::new(),
                    },
                ),
            },
        ))
        .await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let configs: Vec<TurnConfig> = view
        .inputs
        .iter()
        .map(|record| record.config.clone())
        .collect();
    let ran = TurnConfig {
        selection: Some(Box::new(view.selection.clone())),
        mode: Some(PermissionMode::AcceptEdits),
        plan: Some(false),
    };
    // c1 没带配置；c2 与 c3 成批进一轮，各一份；c4 没带配置
    assert_eq!(configs, [ran.clone(), ran]);
    assert_eq!(view.inputs.len(), 2);
}
