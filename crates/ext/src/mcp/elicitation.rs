//! `elicitation/create`（docs/specs/rust-cli/ext.md §3.13）：服务器的请求按 rmcp 的类型解析，换成 kernel-api 的表单或网址，经 hooks
//! 与用户的回答，回 `ElicitResult`。

use std::sync::Arc;

use rmcp::model::{
    ClientResult, ElicitRequestParams, ElicitResult, ElicitationAction as Action,
    ElicitationSchema, EnumSchema, ErrorData, MultiSelectEnumSchema, PrimitiveSchemaDefinition,
    SingleSelectEnumSchema, StringFormat,
};
use rust_cli_kernel_api::hooks::{ElicitationVerdict, HookEventName};
use rust_cli_kernel_api::mcp_ask::{
    Choice, Elicitation, ElicitationAction, ElicitationAnswer, ElicitationKind, FieldKind,
    FormField, TextFormat,
};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use serde_json::{Map, Value, json};

use super::asks::{Asks, Caller, ELICITATION_RESPONSE_NOTIFICATION, Reply, Stop, Stopped};

/// 没声明的模式（TS SDK 客户端的原文）。
const NO_FORM: &str = "Client does not support form-mode elicitation requests";
const NO_URL: &str = "Client does not support URL-mode elicitation requests";
/// rmcp 的请求与 schema 类型标了 `non_exhaustive`：钉死的 3.9.0 没有别的变体（ext.md §3.11.1，升级时重核），以后多出来的当作
/// 不支持，回 `-32602`。
const UNSUPPORTED: &str = "Unsupported elicitation request";

pub(super) async fn serve(asks: &Arc<Asks>, params: Option<Value>, stop: Stop) -> Option<Reply> {
    let raw = params.unwrap_or(Value::Null);
    let request = match serde_json::from_value::<ElicitRequestParams>(raw.clone()) {
        Ok(request) => request,
        Err(error) => return Some(invalid(format!("Invalid params: {error}"))),
    };
    let traits = asks.ports.traits();
    let (elicitation, url_id) = match request {
        ElicitRequestParams::FormElicitationParams {
            message,
            requested_schema,
            ..
        } => {
            if !traits.elicitation_form {
                return Some(invalid(NO_FORM.to_owned()));
            }
            let fields = match fields(&requested_schema) {
                Some(fields) => fields,
                None => return Some(invalid(UNSUPPORTED.to_owned())),
            };
            (
                Elicitation {
                    server: asks.server.clone(),
                    message,
                    kind: ElicitationKind::Form { fields },
                },
                None,
            )
        }
        ElicitRequestParams::UrlElicitationParams {
            message,
            url,
            elicitation_id,
            ..
        } => {
            if !traits.elicitation_url {
                return Some(invalid(NO_URL.to_owned()));
            }
            (
                Elicitation {
                    server: asks.server.clone(),
                    message,
                    kind: ElicitationKind::Url { url },
                },
                Some(elicitation_id),
            )
        }
        _ => return Some(invalid(UNSUPPORTED.to_owned())),
    };
    // 没有在途的调用：没有可以挂的询问（§3.13.2）。
    let Some(caller) = asks.caller() else {
        tracing::debug!(server = %asks.server, "an MCP elicitation came with no tool call in flight; answering cancel");
        return Some(result(ElicitationAnswer::Cancel));
    };
    let stop = stop.on(&caller);
    match elicit(asks, &caller, (elicitation, url_id, &raw), &stop).await {
        Ok(answer) => Some(result(answer)),
        Err(Stopped::Gone) => None,
        Err(Stopped::Called) => Some(result(ElicitationAnswer::Cancel)),
    }
}

async fn elicit(
    asks: &Asks,
    caller: &Arc<Caller>,
    (elicitation, url_id, raw): (Elicitation, Option<String>, &Value),
    stop: &Stop,
) -> Result<ElicitationAnswer, Stopped> {
    let mode = mode(&elicitation);
    let before = asks
        .hooks(
            caller.hook(
                HookEventName::Elicitation,
                hook_input(&elicitation, url_id.as_deref(), raw),
            ),
            stop,
        )
        .await?;
    // 阻塞回 decline；hook 的结论原样回给服务器，不问、不跑结果 hooks（ext.md §3.13.6）。
    if before.blocking_error.is_some() {
        return Ok(ElicitationAnswer::Decline);
    }
    if let Some(verdict) = before.elicitation {
        return Ok(from_verdict(verdict, None));
    }
    let url = matches!(elicitation.kind, ElicitationKind::Url { .. });
    let request = InteractionRequest::McpElicitation {
        tool_use: caller.tool_use.clone(),
        elicitation: Box::new(elicitation),
        asked_by: None,
    };
    let answer = asks
        .ask(caller, request, stop)
        .await?
        .and_then(|answer| ElicitationAnswer::try_from(answer).ok())
        .unwrap_or(ElicitationAnswer::Cancel);
    if url && let (Some(id), ElicitationAnswer::Accept { .. }) = (&url_id, &answer) {
        asks.awaiting(id.clone(), caller.clone());
    }
    let after = asks
        .hooks(
            caller.hook(
                HookEventName::ElicitationResult,
                result_input(&asks.server, url_id.as_deref(), mode, &answer),
            ),
            stop,
        )
        .await?;
    let answer = if after.blocking_error.is_some() {
        ElicitationAnswer::Decline
    } else {
        match after.elicitation {
            Some(verdict) => from_verdict(verdict, Some(&answer)),
            None => answer,
        }
    };
    asks.notify(
        caller,
        format!(
            "Elicitation response for server \"{}\": {}",
            asks.server,
            answer.action().as_str()
        ),
        ELICITATION_RESPONSE_NOTIFICATION,
    );
    Ok(answer)
}

/// 线上的模式名（hooks 的输入）。
fn mode(elicitation: &Elicitation) -> &'static str {
    match elicitation.kind {
        ElicitationKind::Form { .. } => "form",
        ElicitationKind::Url { .. } => "url",
    }
}

/// Elicitation hook 的输入（键序见 ext.md §3.13.6，没有的键不写）：`requested_schema` 是服务器给的原样。
fn hook_input(elicitation: &Elicitation, url_id: Option<&str>, raw: &Value) -> Value {
    let mut input = Map::new();
    input.insert("mcp_server_name".into(), json!(elicitation.server));
    input.insert("message".into(), json!(elicitation.message));
    input.insert("mode".into(), json!(mode(elicitation)));
    if let ElicitationKind::Url { url } = &elicitation.kind {
        input.insert("url".into(), json!(url));
    }
    if let Some(id) = url_id {
        input.insert("elicitation_id".into(), json!(id));
    }
    if let Some(schema) = raw.get("requestedSchema") {
        input.insert("requested_schema".into(), schema.clone());
    }
    Value::Object(input)
}

/// ElicitationResult hook 的输入。
fn result_input(
    server: &str,
    url_id: Option<&str>,
    mode: &str,
    answer: &ElicitationAnswer,
) -> Value {
    let mut input = Map::new();
    input.insert("mcp_server_name".into(), json!(server));
    if let Some(id) = url_id {
        input.insert("elicitation_id".into(), json!(id));
    }
    input.insert("mode".into(), json!(mode));
    input.insert("action".into(), json!(answer.action().as_str()));
    if let ElicitationAnswer::Accept { content } = answer
        && !content.is_empty()
    {
        input.insert("content".into(), Value::Object(content.clone()));
    }
    Value::Object(input)
}

/// hook 的结论 → 回答。结果 hook 没给 `content` 时沿用用户的。
fn from_verdict(
    verdict: ElicitationVerdict,
    original: Option<&ElicitationAnswer>,
) -> ElicitationAnswer {
    match verdict.action {
        ElicitationAction::Accept => ElicitationAnswer::Accept {
            content: verdict.content.unwrap_or_else(|| match original {
                Some(ElicitationAnswer::Accept { content }) => content.clone(),
                _ => Map::new(),
            }),
        },
        ElicitationAction::Decline => ElicitationAnswer::Decline,
        ElicitationAction::Cancel => ElicitationAnswer::Cancel,
    }
}

fn result(answer: ElicitationAnswer) -> Reply {
    let result = match answer {
        ElicitationAnswer::Accept { content } if content.is_empty() => {
            ElicitResult::new(Action::Accept)
        }
        ElicitationAnswer::Accept { content } => {
            ElicitResult::new(Action::Accept).with_content(Value::Object(content))
        }
        ElicitationAnswer::Decline => ElicitResult::new(Action::Decline),
        ElicitationAnswer::Cancel => ElicitResult::new(Action::Cancel),
    };
    Reply::Result(ClientResult::ElicitResult(result))
}

fn invalid(message: String) -> Reply {
    Reply::Error(ErrorData::invalid_params(message, None))
}

/// `requestedSchema` → 表单的字段，按服务器给的顺序；`required` 里不在 `properties` 里的名字不算。有 rmcp 以后才有的
/// 类型时为 None（[`UNSUPPORTED`]）。
fn fields(schema: &ElicitationSchema) -> Option<Vec<FormField>> {
    let required = schema.required.as_deref().unwrap_or_default();
    let order: Vec<&String> = match &schema.property_order {
        Some(order) => order.iter().collect(),
        None => schema.properties.keys().collect(),
    };
    order
        .into_iter()
        .filter_map(|name| {
            schema
                .properties
                .get(name)
                .map(|definition| (name, definition))
        })
        .map(|(name, definition)| {
            let (title, description, kind) = field(definition)?;
            Some(FormField {
                name: name.clone(),
                title,
                description,
                required: required.contains(name),
                kind,
            })
        })
        .collect()
}

type Described = (Option<String>, Option<String>, FieldKind);

fn texts(
    title: Option<&std::borrow::Cow<'static, str>>,
    description: Option<&std::borrow::Cow<'static, str>>,
) -> (Option<String>, Option<String>) {
    (
        title.map(ToString::to_string),
        description.map(ToString::to_string),
    )
}

fn field(definition: &PrimitiveSchemaDefinition) -> Option<Described> {
    Some(match definition {
        PrimitiveSchemaDefinition::String(string) => {
            let (title, description) = texts(string.title.as_ref(), string.description.as_ref());
            let kind = FieldKind::Text {
                format: match string.format {
                    None => None,
                    Some(StringFormat::Email) => Some(TextFormat::Email),
                    Some(StringFormat::Uri) => Some(TextFormat::Uri),
                    Some(StringFormat::Date) => Some(TextFormat::Date),
                    Some(StringFormat::DateTime) => Some(TextFormat::DateTime),
                    Some(_) => return None,
                },
                min_length: string.min_length,
                max_length: string.max_length,
                default: string.default.clone(),
            };
            (title, description, kind)
        }
        PrimitiveSchemaDefinition::Number(number) => {
            let (title, description) = texts(number.title.as_ref(), number.description.as_ref());
            let kind = FieldKind::Number {
                integer: false,
                minimum: number.minimum,
                maximum: number.maximum,
                default: number.default,
            };
            (title, description, kind)
        }
        PrimitiveSchemaDefinition::Integer(integer) => {
            let (title, description) = texts(integer.title.as_ref(), integer.description.as_ref());
            // i64 的边界写成 f64：表单的数字只比大小（超过 2^53 的边界不精确，服务器这样写的边界本来就不实际）。
            let kind = FieldKind::Number {
                integer: true,
                minimum: integer.minimum.map(|n| n as f64),
                maximum: integer.maximum.map(|n| n as f64),
                default: integer.default.map(|n| n as f64),
            };
            (title, description, kind)
        }
        PrimitiveSchemaDefinition::Boolean(boolean) => {
            let (title, description) = texts(boolean.title.as_ref(), boolean.description.as_ref());
            (
                title,
                description,
                FieldKind::Boolean {
                    default: boolean.default,
                },
            )
        }
        PrimitiveSchemaDefinition::Enum(choice) => return enum_field(choice),
        _ => return None,
    })
}

fn plain(values: &[String]) -> Vec<Choice> {
    values
        .iter()
        .map(|value| Choice {
            value: value.clone(),
            label: value.clone(),
        })
        .collect()
}

fn titled(options: &[rmcp::model::ConstTitle]) -> Vec<Choice> {
    options
        .iter()
        .map(|option| Choice {
            value: option.const_.clone(),
            label: option.title.clone(),
        })
        .collect()
}

fn enum_field(schema: &EnumSchema) -> Option<Described> {
    Some(match schema {
        EnumSchema::Single(SingleSelectEnumSchema::Untitled(single)) => {
            let (title, description) = texts(single.title.as_ref(), single.description.as_ref());
            let kind = FieldKind::Choice {
                options: plain(&single.enum_),
                default: single.default.clone(),
            };
            (title, description, kind)
        }
        EnumSchema::Single(SingleSelectEnumSchema::Titled(single)) => {
            let (title, description) = texts(single.title.as_ref(), single.description.as_ref());
            let kind = FieldKind::Choice {
                options: titled(&single.one_of),
                default: single.default.clone(),
            };
            (title, description, kind)
        }
        // 旧写法：`enum` 加 `enumNames`（名字缺了的用值）。
        EnumSchema::Legacy(legacy) => {
            let (title, description) = texts(legacy.title.as_ref(), legacy.description.as_ref());
            let names = legacy.enum_names.as_deref().unwrap_or_default();
            let options = legacy
                .enum_
                .iter()
                .enumerate()
                .map(|(index, value)| Choice {
                    value: value.clone(),
                    label: names.get(index).unwrap_or(value).clone(),
                })
                .collect();
            let kind = FieldKind::Choice {
                options,
                default: legacy.default.clone(),
            };
            (title, description, kind)
        }
        EnumSchema::Multi(MultiSelectEnumSchema::Untitled(multi)) => {
            let (title, description) = texts(multi.title.as_ref(), multi.description.as_ref());
            let kind = FieldKind::Choices {
                options: plain(&multi.items.enum_),
                min_items: multi.min_items,
                max_items: multi.max_items,
                default: multi.default.clone().unwrap_or_default(),
            };
            (title, description, kind)
        }
        EnumSchema::Multi(MultiSelectEnumSchema::Titled(multi)) => {
            let (title, description) = texts(multi.title.as_ref(), multi.description.as_ref());
            let kind = FieldKind::Choices {
                options: titled(&multi.items.any_of),
                min_items: multi.min_items,
                max_items: multi.max_items,
                default: multi.default.clone().unwrap_or_default(),
            };
            (title, description, kind)
        }
        _ => return None,
    })
}
