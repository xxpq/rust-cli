//! 交互（02 §7；protocol-v5.md §15.6）：内核的 `InteractionRequest` → `interactions` 集合的实体；v5 的应答 → 内核的 `Answer`。
//! 实体 id = `tool_use` id（K-X1）：内核保证同一个调用同时只有一个询问，重放、冷读得到同一 id；内核的 `InteractionId` 只在投影里。

use std::collections::BTreeMap;

use rust_cli_kernel_api::interaction::{
    Answer, PermissionAnswer, PlanApprovalAnswer, QuestionAnnotation, QuestionAnswer,
};
use rust_cli_kernel_api::orchestration::{AskedBy, InteractionRequest};
use rust_cli_kernel_api::permission::PermissionMode;
use serde_json::{Value, json};

use crate::generated::session_v1::{
    AskedQuestion, AskedQuestionOption, InteractionAnswerPermission,
    InteractionAnswerPermissionOptionId, InteractionAnswerPlanApproval, InteractionAnswerQuestion,
    InteractionOrigin, InteractionOriginKind, PermissionRule,
};
use crate::publisher::Fields;

use super::mcp_asks;

/// 交互集合（02 §1）。
pub(crate) const INTERACTIONS: &str = "interactions";

/// 交互的种类（`INTERACTION_KINDS`）。
const PERMISSION: &str = "permission";
const QUESTION: &str = "question";
const PLAN_APPROVAL: &str = "planApproval";
// 提问与计划审批的工具名（`toolName`）：内核的请求里没有工具名，这两种只来自这两个工具。
use rust_cli_kernel_api::tool_names::{ASK_USER_QUESTION, EXIT_PLAN_MODE};
/// permission 的选项（`PERMISSION_OPTIONS`）。
const ALLOW_ONCE: &str = "allowOnce";
const ALLOW_ALWAYS: &str = "allowAlways";
const DENY: &str = "deny";
/// 多选的回答接起来的分隔（同 TUI：内核的 `answers` 是问题 → 文字）。
const MULTI_SELECT_SEPARATOR: &str = ", ";

/// 这一问能不能放行并切模式（protocol-v5.md §17.2、§64.3，照 v4 的 `modeSwitch`）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModeSwitch {
    /// 能选切到完全访问（yolo）或自主模式（guarded）：主线程的、不是「只批准这一次」的权限询问。
    Choose,
    /// 只能切到完全访问：guarded 的危险命令（这一问本来只放行一次；staging 的 v4 给它「完全访问」，不给切回 guarded）。
    FullAccessOnly,
}

impl ModeSwitch {
    /// 契约的写法（`PERMISSION_MODE_SWITCHES`）。
    fn name(self) -> &'static str {
        match self {
            Self::Choose => "choose",
            Self::FullAccessOnly => "fullAccessOnly",
        }
    }
}

/// 子代理的询问不给：它的权限模式在启动时定下，选了也只切主会话，那个子代理的下一次调用照样问。
///
/// 修 bug（19 §10 审定第 3 条）：以前恒发 `fullAccessOption: false`，开关开时权限框少了「完全访问」。
fn mode_switch(request: &InteractionRequest) -> Option<ModeSwitch> {
    match request {
        InteractionRequest::Permission {
            asked_by: None,
            once,
            ..
        } => Some(if *once {
            ModeSwitch::FullAccessOnly
        } else {
            ModeSwitch::Choose
        }),
        InteractionRequest::Permission { .. }
        | InteractionRequest::Question { .. }
        | InteractionRequest::PlanApproval { .. }
        | InteractionRequest::McpElicitation { .. }
        | InteractionRequest::McpSampling { .. } => None,
    }
}

/// 实体 id：发起询问的工具调用。
pub(crate) fn entity_id(request: &InteractionRequest) -> String {
    request.tool_use().as_str().to_owned()
}

/// 实体的 `kind`（`INTERACTION_KINDS`）。
pub(crate) fn kind(request: &InteractionRequest) -> &'static str {
    match request {
        InteractionRequest::Permission { .. } => PERMISSION,
        InteractionRequest::Question { .. } => QUESTION,
        InteractionRequest::PlanApproval { .. } => PLAN_APPROVAL,
        InteractionRequest::McpElicitation { .. } => mcp_asks::MCP_ELICITATION,
        InteractionRequest::McpSampling { .. } => mcp_asks::MCP_SAMPLING,
    }
}

/// 询问它的工具名（MCP 的询问是 v4 给同一种交互的名字）。
pub(crate) fn tool_name(request: &InteractionRequest) -> &str {
    match request {
        InteractionRequest::Permission { tool, .. } => tool,
        InteractionRequest::Question { .. } => ASK_USER_QUESTION,
        InteractionRequest::PlanApproval { .. } => EXIT_PLAN_MODE,
        InteractionRequest::McpElicitation { .. } => mcp_asks::MCP_ELICITATION_NAME,
        InteractionRequest::McpSampling { .. } => mcp_asks::MCP_SAMPLING_NAME,
    }
}

/// 工具输入里的一题 → 契约的形状（protocol-v5.md §19.1）。输入在执行管线里已按工具的 schema 校验过，这里按字段取值。
///
/// 修 bug（19-accept.md §15.3 D2）：以前原样发工具的输入，选项只有 `{label, description}`；界面按 v4 的 schema（`value` 必填）
/// 校验时整个题目列表被丢掉，问答卡片不出现。`value` 取 `label`：应答按问题原文给选中的 label（照 v4 的 CLI）。
fn asked_question(question: &Value) -> AskedQuestion {
    let text = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let decoration = |value: &Value, key: &str| value[key].as_str().map(str::to_owned);
    AskedQuestion {
        question: text(question, "question"),
        header: text(question, "header"),
        options: question["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|option| AskedQuestionOption {
                value: text(option, "label"),
                label: text(option, "label"),
                description: decoration(option, "description"),
                preview: decoration(option, "preview"),
            })
            .collect(),
        multi_select: question["multiSelect"].as_bool(),
    }
}

/// 子代理转发的询问的来源（protocol-v5.md §19.2）：v4 的 `zcodeInteractionRequestOriginSchema` 的字段。
fn origin(by: &AskedBy) -> InteractionOrigin {
    InteractionOrigin {
        kind: InteractionOriginKind::Subagent,
        agent_id: by.agent.as_str().to_owned(),
        agent_type: by.agent_type.clone(),
        child_session_id: by.session.as_str().to_owned(),
        description: by.description.clone(),
        parent_session_id: by.parent.as_str().to_owned(),
        parent_tool_call_id: by.tool_use.as_ref().map(|call| call.as_str().to_owned()),
    }
}

/// 实体的字段（不含 id、v）：`{kind, payload}`。
pub(crate) fn fields(request: &InteractionRequest) -> Fields {
    let mut payload = match request {
        InteractionRequest::Permission {
            tool_use,
            tool,
            input,
            message,
            suggestions,
            rule_seed,
            ..
        } => {
            // C6：没有建议规则时不给「始终允许」，client 只渲染给出的选项
            let options: Vec<&str> = if suggestions.is_empty() {
                vec![ALLOW_ONCE, DENY]
            } else {
                vec![ALLOW_ONCE, ALLOW_ALWAYS, DENY]
            };
            let mut detail = json!({"tool": tool, "input": input});
            if let Some(seed) = rule_seed {
                detail["ruleSeed"] = json!(seed);
            }
            let mut payload = json!({
                "toolCallId": tool_use.as_str(),
                "summary": message,
                "detail": detail,
                "options": options,
                // 照 v4：「完全访问」作为单独的一项只在能选的时候给；只能切到完全访问时界面自己补这一项（`pendingInteractionAdapter`）
                "fullAccessOption": mode_switch(request) == Some(ModeSwitch::Choose),
            });
            if let Some(switch) = mode_switch(request) {
                payload["modeSwitch"] = json!(switch.name());
            }
            // 选「始终允许」时记住的规则（§19.3、§23.1）：界面按它显示范围。不是种子：种子只是 TUI 可编辑的起点，常与它不同。
            // 在载荷上、有类型（02 §11.8 第 2 项）：以前在不透明的 `detail` 里，界面自己定了一份形状
            if !suggestions.is_empty() {
                let rules: Vec<PermissionRule> = suggestions
                    .iter()
                    .map(|rule| PermissionRule {
                        tool: rule.tool.clone(),
                        content: rule.content.clone(),
                    })
                    .collect();
                payload["suggestions"] = json!(rules);
            }
            payload
        }
        InteractionRequest::Question {
            tool_use,
            questions,
            ..
        } => {
            let questions: Vec<AskedQuestion> = questions
                .as_array()
                .into_iter()
                .flatten()
                .map(asked_question)
                .collect();
            json!({"questions": questions, "toolCallId": tool_use.as_str(), "toolName": ASK_USER_QUESTION})
        }
        InteractionRequest::PlanApproval { tool_use, plan, .. } => {
            json!({"plan": plan, "toolCallId": tool_use.as_str(), "toolName": EXIT_PLAN_MODE})
        }
        InteractionRequest::McpElicitation {
            tool_use,
            elicitation,
            ..
        } => mcp_asks::elicitation_payload(tool_use.as_str(), elicitation),
        InteractionRequest::McpSampling {
            tool_use, sampling, ..
        } => mcp_asks::sampling_payload(tool_use.as_str(), sampling),
    };
    // 修 bug（19-accept.md §15.3 D8）：以前不写来源，父会话的权限框与问答卡片上看不出是子代理在问
    if let Some(by) = request.asked_by() {
        payload["origin"] = json!(origin(by));
    }
    let mut fields = Fields::new();
    fields.insert("kind".to_owned(), json!(kind(request)));
    fields.insert("payload".to_owned(), payload);
    fields
}

/// 应答不合形状：出问题的字段路径（`answer.<…>`）与原因。
#[derive(Debug)]
pub(crate) struct BadAnswer {
    pub(crate) field: String,
    pub(crate) message: String,
}

pub(super) fn bad(field: &str, message: impl std::fmt::Display) -> BadAnswer {
    BadAnswer {
        field: format!("answer{}{field}", if field.is_empty() { "" } else { "." }),
        message: message.to_string(),
    }
}

/// 先按应答的严格 schema 校验（新字段、新枚举值都拒绝：命令永远严格，02 §11.4），再读成生成的类型。
pub(super) fn decode<T: serde::de::DeserializeOwned>(
    kind: &str,
    answer: &Value,
) -> Result<T, BadAnswer> {
    if let Some(schema) = super::schemas::answer(kind)
        && let Err(error) = schema.validate(answer, "")
    {
        let (field, message) = crate::commands::field_path(&error);
        return Err(bad(&field, message));
    }
    serde_json::from_value(answer.clone()).map_err(|error| bad("", error))
}

/// 一个应答交给内核的形式。
#[derive(Debug, PartialEq)]
pub(crate) enum Reply {
    /// 普通的回答（`ResolveInteraction`）。
    Answer(Answer),
    /// 放行并切到这个模式（`ResolveSwitchingMode`：切模式与放行在内核的一次处理里，§17.2、§64.3）。
    SwitchMode(PermissionMode),
}

/// v5 的应答 → 内核的回答（protocol-v5.md §15.6）。`mode` 是会话现在的权限模式（计划审批的批准只作记录）。
pub(crate) fn answer(
    request: &InteractionRequest,
    answer: &Value,
    mode: PermissionMode,
) -> Result<Reply, BadAnswer> {
    match request {
        InteractionRequest::Permission { suggestions, .. } => {
            let permission: InteractionAnswerPermission = decode(PERMISSION, answer)?;
            Ok(Reply::Answer(Answer::Permission(
                match permission.option_id.resolved() {
                    InteractionAnswerPermissionOptionId::AllowOnce => PermissionAnswer::Allow {
                        remember: Vec::new(),
                    },
                    // C6：没有建议规则时「始终允许」按「允许一次」执行
                    InteractionAnswerPermissionOptionId::AllowAlways => PermissionAnswer::Allow {
                        remember:
                            rust_cli_kernel_api::permission_updates::PermissionUpdate::always_allow(
                                suggestions.clone(),
                            ),
                    },
                    InteractionAnswerPermissionOptionId::Refine => PermissionAnswer::Deny {
                        feedback: permission.refine_text.or(permission.feedback),
                    },
                    InteractionAnswerPermissionOptionId::FullAccess
                        if mode_switch(request).is_some() =>
                    {
                        return Ok(Reply::SwitchMode(PermissionMode::BypassPermissions));
                    }
                    InteractionAnswerPermissionOptionId::AllowAndSwitchGuarded
                        if mode_switch(request) == Some(ModeSwitch::Choose) =>
                    {
                        return Ok(Reply::SwitchMode(PermissionMode::Guarded));
                    }
                    InteractionAnswerPermissionOptionId::FullAccess
                    | InteractionAnswerPermissionOptionId::AllowAndSwitchGuarded => {
                        return Err(bad(
                            "optionId",
                            "switching the mode this way is not offered for this request",
                        ));
                    }
                    InteractionAnswerPermissionOptionId::Deny
                    | InteractionAnswerPermissionOptionId::Unrecognized(_) => {
                        PermissionAnswer::Deny {
                            feedback: permission.feedback,
                        }
                    }
                },
            )))
        }
        InteractionRequest::Question { .. } => {
            let question: InteractionAnswerQuestion = decode(QUESTION, answer)?;
            if question.declined == Some(true) {
                return Ok(Reply::Answer(Answer::Question(QuestionAnswer::Declined)));
            }
            let mut answers = BTreeMap::new();
            for (asked, value) in question.answers {
                let text = match value {
                    Value::String(text) => text,
                    Value::Array(items) => items
                        .iter()
                        .map(|item| item.as_str().map(str::to_owned).ok_or(()))
                        .collect::<Result<Vec<_>, ()>>()
                        .map_err(|()| bad("answers", "a multi-select answer must be strings"))?
                        .join(MULTI_SELECT_SEPARATOR),
                    _ => return Err(bad("answers", "an answer must be a string or strings")),
                };
                answers.insert(asked, text);
            }
            let annotations = question
                .annotations
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(asked, note)| {
                    serde_json::from_value::<QuestionAnnotation>(note)
                        .ok()
                        .map(|note| (asked, note))
                })
                .collect();
            Ok(Reply::Answer(Answer::Question(QuestionAnswer::Answered {
                answers,
                annotations,
                response: None,
                follow_up: false,
                afk_timeout_ms: None,
            })))
        }
        InteractionRequest::PlanApproval { .. } => {
            let plan: InteractionAnswerPlanApproval = decode(PLAN_APPROVAL, answer)?;
            Ok(Reply::Answer(Answer::PlanApproval(if plan.approved {
                PlanApprovalAnswer::Approve { mode }
            } else {
                PlanApprovalAnswer::KeepPlanning {
                    feedback: plan.feedback,
                }
            })))
        }
        InteractionRequest::McpElicitation { elicitation, .. } => {
            mcp_asks::elicitation_answer(elicitation, answer).map(Reply::Answer)
        }
        InteractionRequest::McpSampling { .. } => {
            mcp_asks::sampling_answer(answer).map(Reply::Answer)
        }
    }
}

#[cfg(test)]
#[path = "interactions_tests.rs"]
mod tests;
