//! MCP 服务器的询问（protocol-v5.md §76）：实体过契约的严格变体；应答按表单校验（不合格的给字段路径）、严格（多出的内容拒绝）。

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::interaction::Answer;
use rust_cli_kernel_api::mcp_ask::{
    Choice, Elicitation, ElicitationAnswer, ElicitationKind, FieldKind, FormField, Sampling,
    SamplingAnswer, SamplingMessage, SamplingRole, TextFormat,
};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use serde_json::{Value, json};

use super::super::interactions::{Reply, answer, entity_id, fields};

fn field(name: &str, required: bool, kind: FieldKind) -> FormField {
    FormField {
        name: name.to_owned(),
        title: None,
        description: None,
        required,
        kind,
    }
}

fn form() -> InteractionRequest {
    let fields = vec![
        FormField {
            title: Some("Email".to_owned()),
            description: Some("Where we write".to_owned()),
            ..field(
                "email",
                true,
                FieldKind::Text {
                    format: Some(TextFormat::Email),
                    min_length: None,
                    max_length: Some(40),
                    default: None,
                },
            )
        },
        field(
            "age",
            false,
            FieldKind::Number {
                integer: true,
                minimum: Some(0.0),
                maximum: None,
                default: Some(30.0),
            },
        ),
        field(
            "tags",
            false,
            FieldKind::Choices {
                options: vec![Choice {
                    value: "a".to_owned(),
                    label: "A".to_owned(),
                }],
                min_items: None,
                max_items: Some(1),
                default: Vec::new(),
            },
        ),
    ];
    InteractionRequest::McpElicitation {
        tool_use: ToolUseId::new("toolu_1"),
        elicitation: Box::new(Elicitation {
            server: "srv".to_owned(),
            message: "Who are you?".to_owned(),
            kind: ElicitationKind::Form { fields },
        }),
        asked_by: None,
    }
}

fn sampling() -> InteractionRequest {
    InteractionRequest::McpSampling {
        tool_use: ToolUseId::new("toolu_2"),
        sampling: Box::new(Sampling {
            server: "srv".to_owned(),
            messages: vec![SamplingMessage {
                role: SamplingRole::User,
                text: "Summarize".to_owned(),
            }],
            system: Some("Be brief".to_owned()),
            max_tokens: 100,
        }),
        asked_by: None,
    }
}

/// 实体过契约 `InteractionSchema` 的严格变体（rust 是生产者，严出）。
fn assert_strict_entity(request: &InteractionRequest) {
    let file: Value = serde_json::from_str(include_str!("../../schema/session@1.json")).unwrap();
    let schema = rust_cli_protocol_core::schema::Node::compile(&crate::strict::strict_variant(
        &file["schemas"]["InteractionSchema"],
    ))
    .unwrap();
    let mut entity = serde_json::Map::new();
    entity.insert("id".to_owned(), json!(entity_id(request)));
    entity.insert("v".to_owned(), json!(1));
    entity.extend(fields(request));
    schema
        .validate(&Value::Object(entity), "")
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn mcp_elicitation_fields_follow_the_contract() {
    let request = form();
    assert_strict_entity(&request);
    let fields = fields(&request);
    assert_eq!(fields["kind"], "mcpElicitation");
    assert_eq!(
        fields["payload"],
        json!({
            "toolCallId": "toolu_1",
            "serverName": "srv",
            "message": "Who are you?",
            "mode": "form",
            "fields": [
                {"name": "email", "title": "Email", "description": "Where we write", "required": true, "type": "text", "format": "email", "maxLength": 40},
                {"name": "age", "required": false, "type": "integer", "minimum": 0.0, "default": 30.0},
                {"name": "tags", "required": false, "type": "choices", "maxItems": 1, "options": [{"value": "a", "label": "A"}]}
            ]
        })
    );
    let url = InteractionRequest::McpElicitation {
        tool_use: ToolUseId::new("toolu_3"),
        elicitation: Box::new(Elicitation {
            server: "srv".to_owned(),
            message: "Sign in".to_owned(),
            kind: ElicitationKind::Url {
                url: "https://mcp.example.com/login".to_owned(),
            },
        }),
        asked_by: None,
    };
    assert_strict_entity(&url);
    assert_eq!(fields_of(&url)["mode"], "url");
    assert_eq!(fields_of(&url)["url"], "https://mcp.example.com/login");
}

fn fields_of(request: &InteractionRequest) -> Value {
    fields(request)["payload"].clone()
}

#[test]
fn mcp_sampling_fields_follow_the_contract() {
    let request = sampling();
    assert_strict_entity(&request);
    assert_eq!(
        fields_of(&request),
        json!({
            "toolCallId": "toolu_2",
            "serverName": "srv",
            "messages": [{"role": "user", "text": "Summarize"}],
            "maxTokens": 100,
            "systemPrompt": "Be brief"
        })
    );
}

fn reply(
    request: &InteractionRequest,
    given: Value,
) -> Result<Reply, super::super::interactions::BadAnswer> {
    answer(request, &given, PermissionMode::Default)
}

/// 应答按表单校验：文字交给数字字段换成数字；必填、类型、未知键、带内容的 decline 都拒绝，给字段路径。
#[test]
fn elicitation_answers_are_validated_against_the_form() {
    let request = form();
    assert_eq!(
        reply(
            &request,
            json!({"action": "accept", "content": {"email": "dev@example.com", "age": "7", "tags": ["a"]}})
        )
        .unwrap(),
        Reply::Answer(Answer::McpElicitation(ElicitationAnswer::Accept {
            content: json!({"email": "dev@example.com", "age": 7, "tags": ["a"]})
                .as_object()
                .cloned()
                .unwrap()
        }))
    );
    let missing = reply(&request, json!({"action": "accept", "content": {"age": 7}})).unwrap_err();
    assert_eq!(missing.field, "answer.content.email");
    assert_eq!(missing.message, "This field is required");
    let wrong = reply(
        &request,
        json!({"action": "accept", "content": {"email": "x", "age": -1}}),
    )
    .unwrap_err();
    assert_eq!(wrong.field, "answer.content.email");
    assert_eq!(
        wrong.message,
        "Must be a valid email address, e.g. user@example.com"
    );
    let unknown = reply(
        &request,
        json!({"action": "accept", "content": {"email": "dev@example.com", "x": 1}}),
    )
    .unwrap_err();
    assert_eq!(unknown.field, "answer.content.x");
    assert_eq!(
        reply(&request, json!({"action": "decline"})).unwrap(),
        Reply::Answer(Answer::McpElicitation(ElicitationAnswer::Decline))
    );
    assert_eq!(
        reply(&request, json!({"action": "decline", "content": {}}))
            .unwrap_err()
            .field,
        "answer.content"
    );
    // 命令严格：新的动作、新的键拒绝
    assert!(reply(&request, json!({"action": "skip"})).is_err());
    assert!(reply(&request, json!({"action": "cancel", "extra": 1})).is_err());
}

#[test]
fn sampling_answers_map_to_the_kernel() {
    let request = sampling();
    assert_eq!(
        reply(&request, json!({"allow": true, "session": true})).unwrap(),
        Reply::Answer(Answer::McpSampling(SamplingAnswer::SamplingAllowed {
            session: true
        }))
    );
    assert_eq!(
        reply(&request, json!({"allow": true})).unwrap(),
        Reply::Answer(Answer::McpSampling(SamplingAnswer::SamplingAllowed {
            session: false
        }))
    );
    assert_eq!(
        reply(&request, json!({"allow": false})).unwrap(),
        Reply::Answer(Answer::McpSampling(SamplingAnswer::SamplingDenied))
    );
    assert_eq!(
        reply(&request, json!({"allow": false, "session": true}))
            .unwrap_err()
            .field,
        "answer.session"
    );
}
