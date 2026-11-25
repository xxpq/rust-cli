//! 交互的应答（protocol-v5.md §15.6）：按 kind 换成内核的回答；应答严格（新枚举值、新字段拒绝）；C6 的「始终允许」。

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::interaction::{
    Answer, PermissionAnswer, PlanApprovalAnswer, QuestionAnswer,
};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule};
use serde_json::{Value, json};

use super::*;

fn permission(suggestions: Vec<PermissionRule>) -> InteractionRequest {
    InteractionRequest::Permission {
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "touch a"}),
        message: "Run touch a?".to_owned(),
        suggestions,
        rule_seed: None,
        asked_by: None,
        machine: Default::default(),
        once: false,
    }
}

#[test]
fn permission_answers_map_and_allow_always_needs_a_suggestion() {
    let rule = PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("touch:*".to_owned()),
    };
    let with = permission(vec![rule.clone()]);
    assert_eq!(
        fields(&with)["payload"]["options"],
        json!(["allowOnce", "allowAlways", "deny"])
    );
    assert_eq!(
        answer(
            &with,
            &json!({"optionId": "allowAlways"}),
            PermissionMode::Default
        )
        .unwrap(),
        Reply::Answer(Answer::Permission(PermissionAnswer::Allow {
            remember: rust_cli_kernel_api::permission_updates::PermissionUpdate::always_allow(
                vec![rule]
            )
        }))
    );
    // C6：没有建议规则时「始终允许」按「允许一次」执行
    assert_eq!(
        answer(
            &permission(Vec::new()),
            &json!({"optionId": "allowAlways"}),
            PermissionMode::Default
        )
        .unwrap(),
        Reply::Answer(Answer::Permission(PermissionAnswer::Allow {
            remember: Vec::new()
        }))
    );
    assert_eq!(
        answer(
            &with,
            &json!({"optionId": "deny", "feedback": "no"}),
            PermissionMode::Default
        )
        .unwrap(),
        Reply::Answer(Answer::Permission(PermissionAnswer::Deny {
            feedback: Some("no".to_owned())
        }))
    );
}

#[test]
fn answers_are_strict() {
    let request = permission(Vec::new());
    let unknown = answer(
        &request,
        &json!({"optionId": "maybe"}),
        PermissionMode::Default,
    )
    .unwrap_err();
    assert_eq!(unknown.field, "answer.optionId");
    let extra = answer(
        &request,
        &json!({"optionId": "deny", "why": "x"}),
        PermissionMode::Default,
    )
    .unwrap_err();
    assert_eq!(extra.field, "answer.why");
}

/// 「放行并切模式」（protocol-v5.md §17.2、§64.3）：主线程的权限询问能选切到完全访问或自主模式（`modeSwitch: choose`），选哪个都交给内核的一条命令；
/// 子代理的询问不给，应答哪个都是不合形状。
#[test]
fn full_access_is_offered_for_the_main_thread_only() {
    let main = permission(Vec::new());
    assert_eq!(fields(&main)["payload"]["fullAccessOption"], json!(true));
    assert_eq!(fields(&main)["payload"]["modeSwitch"], json!("choose"));
    assert_eq!(
        answer(
            &main,
            &json!({"optionId": "fullAccess"}),
            PermissionMode::Default
        )
        .unwrap(),
        Reply::SwitchMode(PermissionMode::BypassPermissions)
    );
    assert_eq!(
        answer(
            &main,
            &json!({"optionId": "allowAndSwitchGuarded"}),
            PermissionMode::Default
        )
        .unwrap(),
        Reply::SwitchMode(PermissionMode::Guarded)
    );
    let InteractionRequest::Permission {
        tool_use,
        tool,
        input,
        message,
        suggestions,
        rule_seed,
        machine,
        ..
    } = main
    else {
        unreachable!("permission() builds a permission request");
    };
    let child = InteractionRequest::Permission {
        tool_use,
        tool,
        input,
        message,
        suggestions,
        rule_seed,
        asked_by: Some(Box::new(rust_cli_kernel_api::testing::asked_by("Explore"))),
        machine,
        once: false,
    };
    assert_eq!(fields(&child)["payload"]["fullAccessOption"], json!(false));
    assert!(fields(&child)["payload"].get("modeSwitch").is_none());
    for option in ["fullAccess", "allowAndSwitchGuarded"] {
        let refused = answer(
            &child,
            &json!({ "optionId": option }),
            PermissionMode::Default,
        )
        .unwrap_err();
        assert_eq!(refused.field, "answer.optionId");
    }
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_q"),
        questions: json!([]),
        asked_by: None,
    };
    assert!(
        fields(&question)["payload"]
            .get("fullAccessOption")
            .is_none()
    );
}

#[test]
fn questions_and_plans_map_to_their_answers() {
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_q"),
        questions: json!([{"question": "Pick?"}]),
        asked_by: None,
    };
    let Reply::Answer(Answer::Question(QuestionAnswer::Answered { answers, .. })) = answer(
        &question,
        &json!({"answers": {"Pick?": ["a", "b"]}}),
        PermissionMode::Default,
    )
    .unwrap() else {
        panic!("not answered")
    };
    assert_eq!(answers["Pick?"], "a, b");
    assert_eq!(
        answer(
            &question,
            &json!({"answers": {}, "declined": true}),
            PermissionMode::Default
        )
        .unwrap(),
        Reply::Answer(Answer::Question(QuestionAnswer::Declined))
    );
    let plan = InteractionRequest::PlanApproval {
        tool_use: ToolUseId::new("toolu_p"),
        plan: "do it".to_owned(),
        asked_by: None,
    };
    assert_eq!(
        answer(
            &plan,
            &json!({"approved": true}),
            PermissionMode::AcceptEdits
        )
        .unwrap(),
        Reply::Answer(Answer::PlanApproval(PlanApprovalAnswer::Approve {
            mode: PermissionMode::AcceptEdits
        }))
    );
}

/// guarded 的危险命令只批准这一次：照 staging 的 v4 只能切到完全访问（`modeSwitch: fullAccessOnly`，§64.3；2026-11-05 照当时的 v4 不给，
/// v4 改了之后跟着改）。「完全访问」不作为单独的一项（界面自己补），切回 guarded 不可选。
#[test]
fn a_one_time_ask_only_offers_full_access() {
    let InteractionRequest::Permission {
        tool_use,
        tool,
        input,
        message,
        machine,
        ..
    } = permission(Vec::new())
    else {
        unreachable!("permission() builds a permission request");
    };
    let once = InteractionRequest::Permission {
        tool_use,
        tool,
        input,
        message,
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: None,
        machine,
        once: true,
    };
    assert_eq!(fields(&once)["payload"]["fullAccessOption"], json!(false));
    assert_eq!(
        fields(&once)["payload"]["modeSwitch"],
        json!("fullAccessOnly")
    );
    assert_eq!(
        fields(&once)["payload"]["options"],
        json!(["allowOnce", "deny"])
    );
    assert_eq!(
        answer(
            &once,
            &json!({"optionId": "fullAccess"}),
            PermissionMode::Guarded
        )
        .unwrap(),
        Reply::SwitchMode(PermissionMode::BypassPermissions)
    );
    let refused = answer(
        &once,
        &json!({"optionId": "allowAndSwitchGuarded"}),
        PermissionMode::Guarded,
    )
    .unwrap_err();
    assert_eq!(refused.field, "answer.optionId");
}

/// 实体（`{id, v, kind, payload}`）过契约 `InteractionSchema` 的严格变体：多出的键、缺的必填字段都不行（rust 是生产者，严出）。
fn assert_strict_entity(request: &InteractionRequest) {
    let file: serde_json::Value =
        serde_json::from_str(include_str!("../../schema/session@1.json")).unwrap();
    let schema = rust_cli_protocol_core::schema::Node::compile(&crate::strict::strict_variant(
        &file["schemas"]["InteractionSchema"],
    ))
    .unwrap();
    let mut entity = serde_json::Map::new();
    entity.insert("id".to_owned(), json!(entity_id(request)));
    entity.insert("v".to_owned(), json!(1));
    entity.extend(fields(request));
    schema
        .validate(&serde_json::Value::Object(entity), "")
        .unwrap_or_else(|error| panic!("{error}"));
}

/// 修 bug（19-accept.md §15.3 D2）：以前原样发工具的输入，选项只有 `{label, description}`，界面按 v4 的 schema（`value` 必填）
/// 校验时整个题目列表被丢掉，问答卡片不出现。现在按契约的形状发，`value` 取 `label`（照 v4 的 CLI）。
#[test]
fn questions_take_the_contract_shape() {
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_q"),
        questions: json!([{
            "question": "Which color?",
            "header": "Color",
            "multiSelect": true,
            "options": [
                {"label": "Red", "description": "warm"},
                {"label": "Blue", "description": "cold", "preview": "#00f"}
            ]
        }]),
        asked_by: None,
    };
    assert_eq!(
        fields(&question)["payload"]["questions"],
        json!([{
            "question": "Which color?",
            "header": "Color",
            "options": [
                {"value": "Red", "label": "Red", "description": "warm"},
                {"value": "Blue", "label": "Blue", "description": "cold", "preview": "#00f"}
            ],
            "multiSelect": true
        }])
    );
    assert_strict_entity(&question);
}

/// 子代理转发的询问（protocol-v5.md §19.2）：三种都带 `origin`（v4 的子代理来源）；主线程的没有。
#[test]
fn subagent_asks_carry_their_origin() {
    let by = rust_cli_kernel_api::testing::asked_by("Explore");
    let origin = json!({
        "kind": "subagent",
        "agentId": "agent_test",
        "agentType": "Explore",
        "childSessionId": "sess_subagent_test",
        "description": "Test task",
        "parentSessionId": "sess_parent_test",
        "parentToolCallId": "toolu_agent_test"
    });
    let mut asks = [
        permission(Vec::new()),
        InteractionRequest::Question {
            tool_use: ToolUseId::new("toolu_q"),
            questions: json!([]),
            asked_by: None,
        },
        InteractionRequest::PlanApproval {
            tool_use: ToolUseId::new("toolu_p"),
            plan: "do it".to_owned(),
            asked_by: None,
        },
    ];
    for ask in &mut asks {
        assert!(fields(ask)["payload"].get("origin").is_none(), "{ask:?}");
        *ask.asked_by_mut() = Some(Box::new(by.clone()));
        assert_eq!(fields(ask)["payload"]["origin"], origin, "{ask:?}");
        assert_strict_entity(ask);
    }
}

/// 「始终允许」记住的规则（protocol-v5.md §19.3、§23.1）：载荷的 `suggestions` 就是选它时交给内核的建议规则，界面按它显示范围；
/// 没有建议规则时不写。种子（`ruleSeed`）照旧在 `detail` 里，它不是记住的规则。
#[test]
fn the_payload_lists_the_rules_that_allow_always_remembers() {
    let rule = PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("docker compose *".to_owned()),
    };
    let InteractionRequest::Permission {
        tool_use,
        tool,
        message,
        machine,
        ..
    } = permission(Vec::new())
    else {
        unreachable!("permission() builds a permission request");
    };
    let with = InteractionRequest::Permission {
        tool_use,
        tool,
        input: json!({"command": "docker compose up -d"}),
        message,
        suggestions: vec![rule.clone()],
        rule_seed: Some("docker compose up *".to_owned()),
        asked_by: None,
        machine,
        once: false,
    };
    // 记住的规则在载荷上、有类型（02 §11.8 第 2 项）；`detail` 里只留工具请求的细节
    let payload = &fields(&with)["payload"];
    assert_eq!(
        payload["suggestions"],
        json!([{"tool": "Bash", "content": "docker compose *"}])
    );
    assert_eq!(payload["detail"]["suggestions"], Value::Null);
    assert_eq!(payload["detail"]["ruleSeed"], json!("docker compose up *"));
    assert_eq!(
        answer(
            &with,
            &json!({"optionId": "allowAlways"}),
            PermissionMode::Default
        )
        .unwrap(),
        Reply::Answer(Answer::Permission(PermissionAnswer::Allow {
            remember: rust_cli_kernel_api::permission_updates::PermissionUpdate::always_allow(
                vec![rule]
            )
        }))
    );
    assert!(
        fields(&permission(Vec::new()))["payload"]
            .get("suggestions")
            .is_none()
    );
}
