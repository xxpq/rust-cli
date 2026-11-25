//! MCP 服务器的询问（docs/specs/rust-cli/protocol-v5.md §76）：内核的 `McpElicitation` / `McpSampling` → `mcpElicitation` /
//! `mcpSampling` 实体的载荷；v5 的应答 → 内核的回答。表单的校验用 kernel-api 的 `mcp_ask::accept`（与 TUI 同一份规则与文字）：
//! 不合格的应答被拒，实体留着可以再答。

use rust_cli_kernel_api::interaction::Answer;
use rust_cli_kernel_api::mcp_ask::{
    Elicitation, ElicitationAnswer, ElicitationKind, FieldKind, FormField, Sampling,
    SamplingAnswer, SamplingRole, TextFormat, accept,
};
use serde_json::{Map, Value, json};

use crate::generated::session_v1::{
    ElicitationChoice, ElicitationField, ElicitationFieldFormat, ElicitationFieldType,
    InteractionAnswerMcpElicitation, InteractionAnswerMcpElicitationAction,
    InteractionAnswerMcpSampling, SamplingTurn, SamplingTurnRole,
};

use super::interactions::{BadAnswer, bad, decode};

/// 两种 kind（`INTERACTION_KINDS`）。
pub(super) const MCP_ELICITATION: &str = "mcpElicitation";
pub(super) const MCP_SAMPLING: &str = "mcpSampling";
/// 会话列表里待回答交互的「工具名」（v4 的 CLI 给同一种交互的名字）。
pub(super) const MCP_ELICITATION_NAME: &str = "McpElicitation";
pub(super) const MCP_SAMPLING_NAME: &str = "McpSampling";

/// `mcpElicitation` 的载荷（不含 `origin`，由调用方补）。
pub(super) fn elicitation_payload(tool_use: &str, elicitation: &Elicitation) -> Value {
    let (mode, fields, url) = match &elicitation.kind {
        ElicitationKind::Form { fields } => ("form", fields.iter().map(field).collect(), None),
        ElicitationKind::Url { url } => ("url", Vec::new(), Some(url.as_str())),
    };
    let mut payload = json!({
        "toolCallId": tool_use,
        "serverName": elicitation.server,
        "message": elicitation.message,
        "mode": mode,
        "fields": fields,
    });
    if let Some(url) = url {
        payload["url"] = json!(url);
    }
    payload
}

/// 一个字段 → 契约的形状：`Number{integer}` 写成 `number` / `integer` 两种 `type`。
fn field(field: &FormField) -> ElicitationField {
    let mut out = ElicitationField {
        name: field.name.clone(),
        title: field.title.clone(),
        description: field.description.clone(),
        required: field.required,
        r#type: ElicitationFieldType::Text,
        format: None,
        min_length: None,
        max_length: None,
        minimum: None,
        maximum: None,
        min_items: None,
        max_items: None,
        options: None,
        default: field.initial(),
    };
    let choices = |options: &[rust_cli_kernel_api::mcp_ask::Choice]| {
        Some(
            options
                .iter()
                .map(|option| ElicitationChoice {
                    value: option.value.clone(),
                    label: option.label.clone(),
                })
                .collect(),
        )
    };
    match &field.kind {
        FieldKind::Text {
            format,
            min_length,
            max_length,
            ..
        } => {
            out.format = format.map(|format| match format {
                TextFormat::Email => ElicitationFieldFormat::Email,
                TextFormat::Uri => ElicitationFieldFormat::Uri,
                TextFormat::Date => ElicitationFieldFormat::Date,
                TextFormat::DateTime => ElicitationFieldFormat::DateTime,
            });
            out.min_length = min_length.map(u64::from);
            out.max_length = max_length.map(u64::from);
        }
        FieldKind::Number {
            integer,
            minimum,
            maximum,
            ..
        } => {
            out.r#type = if *integer {
                ElicitationFieldType::Integer
            } else {
                ElicitationFieldType::Number
            };
            out.minimum = *minimum;
            out.maximum = *maximum;
        }
        FieldKind::Boolean { .. } => out.r#type = ElicitationFieldType::Boolean,
        FieldKind::Choice { options, .. } => {
            out.r#type = ElicitationFieldType::Choice;
            out.options = choices(options);
        }
        FieldKind::Choices {
            options,
            min_items,
            max_items,
            ..
        } => {
            out.r#type = ElicitationFieldType::Choices;
            out.options = choices(options);
            out.min_items = *min_items;
            out.max_items = *max_items;
        }
    }
    out
}

/// `mcpSampling` 的载荷（不含 `origin`）。
pub(super) fn sampling_payload(tool_use: &str, sampling: &Sampling) -> Value {
    let messages: Vec<SamplingTurn> = sampling
        .messages
        .iter()
        .map(|message| SamplingTurn {
            role: match message.role {
                SamplingRole::User => SamplingTurnRole::User,
                SamplingRole::Assistant => SamplingTurnRole::Assistant,
            },
            text: message.text.clone(),
        })
        .collect();
    let mut payload = json!({
        "toolCallId": tool_use,
        "serverName": sampling.server,
        "messages": messages,
        "maxTokens": sampling.max_tokens,
    });
    if let Some(system) = &sampling.system {
        payload["systemPrompt"] = json!(system);
    }
    payload
}

/// v5 的应答 → elicitation 的回答：accept 的表单内容按 `accept` 校验并写成服务器要的类型；网址模式与 decline、cancel 不带内容（命令严格）。
pub(super) fn elicitation_answer(
    elicitation: &Elicitation,
    answer: &Value,
) -> Result<Answer, BadAnswer> {
    let answer: InteractionAnswerMcpElicitation = decode(MCP_ELICITATION, answer)?;
    let content = answer
        .content
        .map(|content| content.into_iter().collect::<Map<_, _>>());
    let reply = match (answer.action, &elicitation.kind, content) {
        (
            InteractionAnswerMcpElicitationAction::Accept,
            ElicitationKind::Form { fields },
            content,
        ) => {
            let content = accept(fields, &content.unwrap_or_default())
                .map_err(|error| bad(&format!("content.{}", error.field), error.message))?;
            ElicitationAnswer::Accept { content }
        }
        (InteractionAnswerMcpElicitationAction::Accept, ElicitationKind::Url { .. }, None) => {
            ElicitationAnswer::Accept {
                content: Map::new(),
            }
        }
        (InteractionAnswerMcpElicitationAction::Decline, _, None) => ElicitationAnswer::Decline,
        (InteractionAnswerMcpElicitationAction::Cancel, _, None) => ElicitationAnswer::Cancel,
        (_, _, Some(_)) => {
            return Err(bad("content", "only an accepted form carries content"));
        }
    };
    Ok(Answer::McpElicitation(reply))
}

/// v5 的应答 → sampling 确认的回答：`session` 只在 allow 时。
pub(super) fn sampling_answer(answer: &Value) -> Result<Answer, BadAnswer> {
    let answer: InteractionAnswerMcpSampling = decode(MCP_SAMPLING, answer)?;
    let reply = match (answer.allow, answer.session) {
        (true, session) => SamplingAnswer::SamplingAllowed {
            session: session == Some(true),
        },
        (false, None) => SamplingAnswer::SamplingDenied,
        (false, Some(_)) => {
            return Err(bad(
                "session",
                "only an allowed request is remembered for the session",
            ));
        }
    };
    Ok(Answer::McpSampling(reply))
}

#[cfg(test)]
#[path = "mcp_asks_tests.rs"]
mod tests;
