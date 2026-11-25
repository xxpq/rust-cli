//! `input.send` 正文之外的字段 ↔ 内核的 `InputExtras`（02 §11.2 C1、§11.6；protocol-v5.md §15.4.1）：命令里读成内核的事实，
//! 行与队列项里按同一个形状写回。读与写在一处，两边的形状不会走开。

use std::path::Path;

use rust_cli_kernel_api::input_extras::{
    AttachedFile, InputExtras, InputRecord, PageState, Quote, Relay, TurnConfig, UploadedFile,
};
use rust_cli_kernel_api::message::{ContentBlock, ImageSource};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::turn_execution::{ExecutionFallback, FallbackRule, TurnExecution};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::commands::{Refused, rejected};
use crate::consts::{GEN_UI_CONTEXT_MAX_BYTES, GEN_UI_STATE_MAX_BYTES};
use crate::generated::contract::codes;
use crate::generated::session_v1::{
    CommandInputSend, CommandInputSendGenUiContextItem, CommandInputSendModelExecution,
    CommandInputSendModelSelection, CommandInputSendQuotesItem, CommandInputSendSource,
    ExecutionCredential, InputQuote, InputSource, InputSourceGenUi, InputSourcePluginUi,
};
use crate::publisher::{Fields, Publisher};
use crate::tolerant::Opaque;

/// 附件在命令与行里的形状：v4 的 `AttachmentRef`（`packages/shared/src/zcode-protocol-v4/attachment-ref.ts`），契约里是
/// 不透明 JSON。命令里多出的 `sourceKind`、`messageCount` 不读（接受后丢弃，C1），`previewRef` 读了也不用。读面的授权（[`shown_attachment`]）
/// 按行里的同一个形状读。行的 `previewRef` 是本机附件这一轮写下的 Artifact（§45.4）。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentRef {
    r#ref: String,
    file_name: String,
    mime: String,
    bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview_ref: Option<String>,
}

/// 命令里页面状态的字段（`precondition.payloadTooLarge{field}`）。
const GEN_UI_CONTEXT_FIELD: &str = "genUiContext";

/// `input.send` 的正文之外的事实（§15.4.1）。引用与来源的形状、条数与正文长度由命令的 schema 校验过；页面状态的字节上限在这里守。
pub(crate) fn extras(send: &CommandInputSend) -> Result<InputExtras, Refused> {
    let (files, uploads) = files(&send.attachments);
    Ok(InputExtras {
        files,
        uploads,
        quotes: send.quotes.iter().flatten().map(quote).collect(),
        relay: send.source.as_ref().map(relay),
        pages: pages(send.gen_ui_context.as_deref().unwrap_or_default())?,
        // 每轮的工具禁用名单（K-C2，§15.4.1）：这一轮与这一轮里启动的子代理都不许用
        disallowed_tools: send.tool_disallowlist.clone().unwrap_or_default(),
        config: config(send)?,
        execution: send
            .model_execution
            .as_ref()
            .map(|execution| Box::new(turn_execution(execution))),
        // 抽卡时的显示事实（§46.0 第 1 条）：命令里严格读过（契约的 HighspeedTurnMeta），原样成这条输入的注记
        note: send
            .highspeed_meta
            .as_ref()
            .map(|meta| Box::new(json!(meta))),
    })
}

/// 别的命令里与 `input.send` 同形的那几个字段（`turn.edit`、`queue.edit`、`queue.applyConfig`，§53.2、§64.1）：去掉 `drop` 里只有那条命令才有的键、
/// 正文写成 `text`，读成 `input.send` 的参数，之后用同一套换法（[`extras`]）。参数已按那条命令的 schema 严格校验过。
pub(super) fn as_send(
    payload: &Value,
    drop: &[&str],
    text: &str,
) -> Result<CommandInputSend, Refused> {
    let mut fields = payload.as_object().cloned().unwrap_or_default();
    for key in drop {
        fields.remove(*key);
    }
    fields.insert(TEXT_KEY.to_owned(), json!(text));
    serde_json::from_value(Value::Object(fields)).map_err(|error| {
        rejected(
            codes::PRECONDITION_INVALID_PAYLOAD,
            None,
            Some(error.to_string()),
        )
    })
}

/// 这一轮的执行覆盖（protocol-v5.md §38，23 §3.3）：执行选择同 `modelSelection` 的换法；规则原样（原因是线上的写法，内核不解释）；
/// 凭据引用写成判别联合的 JSON 文字交给内核，取凭据时 [`credential`] 读回（同 `SuppliedAuth::binding`：内核只原样带）。
fn turn_execution(execution: &CommandInputSendModelExecution) -> TurnExecution {
    let chosen = &execution.selection;
    TurnExecution {
        selection: ModelSelection {
            reasoning: chosen
                .options
                .as_ref()
                .and_then(|options| options.reasoning_level.clone())
                .filter(|level| !level.is_empty()),
            ..ModelSelection::new(chosen.provider_id.clone(), chosen.model_id.clone())
        },
        credential: execution
            .credential
            .as_ref()
            .map(|credential| json!(credential).to_string()),
        fallback: execution
            .fallback
            .as_ref()
            .map(|fallback| ExecutionFallback {
                rules: fallback
                    .rules
                    .iter()
                    .map(|rule| FallbackRule {
                        reason: rule.reason.as_str().to_owned(),
                        provider_code: rule.provider_error_code.clone(),
                    })
                    .collect(),
                unavailable: fallback.unavailable.as_str().to_owned(),
            }),
    }
}

/// 内核带回来的凭据引用 → 契约的判别联合（[`turn_execution`] 写的文字）。读不回的文字不是这一端写的，按没有引用处理。
pub(crate) fn credential(reference: &str) -> Option<ExecutionCredential> {
    serde_json::from_str(reference).ok()
}

/// 输入框的配置（K-C1-INTENT，protocol-v5.md §18.1）：模式名照 v4 的 `resolveExecutionState` 换——给了模式、没给 plan 时 plan 关；
/// 旧写法 `plan` 只开 plan、模式不给；不认识的模式名拒绝（命令参数严格，11 §5）。没给的项不给（开回合时用会话那时的值）。
fn config(send: &CommandInputSend) -> Result<TurnConfig, Refused> {
    let (mode, plan) = match send.mode.as_deref() {
        None => (None, send.plan_enabled),
        Some(name) => {
            let Some((mode, legacy_plan)) = super::commands::mode(name) else {
                return Err(rejected(
                    codes::PRECONDITION_INVALID_MODE,
                    Some(super::commands::MODE_FIELD.to_owned()),
                    None,
                ));
            };
            (
                (!legacy_plan).then_some(mode),
                Some(send.plan_enabled.unwrap_or(legacy_plan)),
            )
        }
    };
    Ok(TurnConfig {
        selection: send
            .model_selection
            .as_ref()
            .map(|chosen| Box::new(selection(chosen))),
        mode,
        plan,
    })
}

/// 命令里的稀疏模型选择 → 内核的选择（`input.send` 与 `session.compact` 同一个 schema，§18.1、§23.2）。
pub(super) fn selection(chosen: &CommandInputSendModelSelection) -> ModelSelection {
    ModelSelection {
        reasoning: chosen
            .options
            .as_ref()
            .and_then(|options| options.reasoning_level.clone())
            .filter(|level| !level.is_empty()),
        ..ModelSelection::new(chosen.provider_id.clone(), chosen.model_id.clone())
    }
}

fn quote(quote: &CommandInputSendQuotesItem) -> Quote {
    Quote {
        text: quote.text.clone(),
        path: quote.path.clone(),
        sender_name: quote.sender_name.clone(),
        sender_id: quote.sender_id.clone(),
        message_id: quote.message_id.clone(),
        sent_at_ms: quote.sent_at,
    }
}

fn relay(source: &CommandInputSendSource) -> Relay {
    match source {
        CommandInputSendSource::PluginUi(plugin) => Relay::Plugin {
            plugin: plugin.plugin_id.clone(),
            tool_call: plugin.tool_call_id.clone(),
        },
        CommandInputSendSource::GenUi(page) => Relay::Page {
            path: page.path.clone(),
            title: page.title.clone(),
        },
    }
}

/// Gen UI 页面状态（K-P3）：每项的 `state` 与全部的 JSON 文字各有字节上限（同 v4 `genUiModelContextSchema`），超出整条拒绝。
fn pages(items: &[CommandInputSendGenUiContextItem]) -> Result<Vec<PageState>, Refused> {
    let too_large = || {
        rejected(
            codes::PRECONDITION_PAYLOAD_TOO_LARGE,
            Some(GEN_UI_CONTEXT_FIELD.to_owned()),
            Some(format!(
                "page state is limited to {GEN_UI_STATE_MAX_BYTES} bytes an item and {GEN_UI_CONTEXT_MAX_BYTES} in all"
            )),
        )
    };
    if items
        .iter()
        .any(|item| item.state.to_string().len() > GEN_UI_STATE_MAX_BYTES)
        || json!(items).to_string().len() > GEN_UI_CONTEXT_MAX_BYTES
    {
        return Err(too_large());
    }
    Ok(items
        .iter()
        .map(|item| PageState {
            path: item.path.clone(),
            state: item.state.clone(),
        })
        .collect())
}

/// 附件（K-C1、§39.5）：`ref` 是本机绝对路径的 `AttachmentRef` 是附上的本机文件；是 Artifact URI 的是上传的文件（attachments@1
/// 的 commit 给出的引用，§39）；别的接受后丢弃（C1，契约里附件是不透明 JSON）。
fn files(attachments: &[Opaque]) -> (Vec<AttachedFile>, Vec<UploadedFile>) {
    let (mut files, mut uploads) = (Vec::new(), Vec::new());
    for (index, attachment) in attachments.iter().enumerate() {
        let Ok(attachment) = serde_json::from_value::<AttachmentRef>(attachment.clone()) else {
            tracing::debug!(
                event = "protocol.input.dropped",
                index,
                "dropped an attachment that is not an AttachmentRef"
            );
            continue;
        };
        if Path::new(&attachment.r#ref).is_absolute() {
            files.push(AttachedFile {
                path: attachment.r#ref,
                name: attachment.file_name,
                media_type: attachment.mime,
                size: attachment.bytes,
            });
        } else if let Some((session, artifact)) =
            rust_cli_kernel_api::artifact_uri::parse(&attachment.r#ref)
        {
            uploads.push(UploadedFile {
                session,
                artifact,
                name: attachment.file_name,
                media_type: attachment.mime,
                size: attachment.bytes,
            });
        } else {
            tracing::debug!(
                event = "protocol.input.dropped",
                index,
                "dropped an attachment that is neither a local file nor an upload"
            );
        }
    }
    (files, uploads)
}

/// 行（`userInput.attachments`）与队列项的 `attachments`：命令里的同一个形状（v4 的行也是 `AttachmentRef`）。先是本机文件，再是上传的
/// 文件（各自按给出的顺序，§39.5）；上传的文件的 `ref` 写回它的 URI。
pub(crate) fn attachments(files: &[AttachedFile], uploads: &[UploadedFile]) -> Value {
    shown(files, uploads, &[])
}

/// 同 [`attachments`]；本机附上的图片与 PDF 在这条用户消息里有这一轮写下的 Artifact 时带 `previewRef`（§45.4：块的 origin 的路径就是行的
/// `ref`；Node 的读面按（消息，附件序号）找 file part，rust 的 file part 与行的附件不一一对应，所以按路径对上）。
fn shown(files: &[AttachedFile], uploads: &[UploadedFile], content: &[ContentBlock]) -> Value {
    let preview = |path: &str| {
        content.iter().find_map(|block| match block {
            ContentBlock::Image {
                source: ImageSource::Artifact { artifact, .. },
                origin: Some(origin),
            }
            | ContentBlock::Document {
                source: ImageSource::Artifact { artifact, .. },
                origin: Some(origin),
            } if origin.path.as_deref() == Some(path) => Some(
                rust_cli_kernel_api::artifact_uri::uri(&artifact.session, &artifact.id),
            ),
            _ => None,
        })
    };
    let shown = |reference: String, name: &str, mime: &str, bytes: u64, preview_ref| {
        json!(AttachmentRef {
            r#ref: reference,
            file_name: name.to_owned(),
            mime: mime.to_owned(),
            bytes,
            preview_ref,
        })
    };
    Value::Array(
        files
            .iter()
            .map(|file| {
                shown(
                    file.path.clone(),
                    &file.name,
                    &file.media_type,
                    file.size,
                    preview(&file.path),
                )
            })
            .chain(uploads.iter().map(|upload| {
                shown(
                    upload.uri(),
                    &upload.name,
                    &upload.media_type,
                    upload.size,
                    None,
                )
            }))
            .collect(),
    )
}

impl AttachmentRef {
    /// 读面的引用是这个附件：它的 `ref`，或者这一轮写下的 Artifact（`previewRef`，§45.4）。
    fn is(&self, reference: &str) -> bool {
        self.r#ref == reference || self.preview_ref.as_deref() == Some(reference)
    }
}

/// 只给 `ref` 时能读的类型（05 §1.A：带行与序号的精确定位才放开任意类型）。
const PREVIEWABLE: [&str; 3] = ["image/", "video/", rust_cli_base::proto::mime::PDF];

/// 读面的授权（§39.4）：这个会话的投影里带这个引用的附件，交回它的类型。带 `(行, 序号)` 时只看那一行（`userInput`）的那一个附件；
/// 只有引用时扫全部用户行，类型限图片、视频、PDF。没有时为 None。
pub(crate) fn shown_attachment(
    publisher: &Publisher,
    reference: &str,
    at: Option<(&str, usize)>,
) -> Option<String> {
    let user_row = |fields: &&Fields| {
        fields.get(super::rows::keys::KIND).and_then(Value::as_str)
            == Some(super::rows::kinds::USER_INPUT)
    };
    let shown = |fields: &Fields| -> Vec<AttachmentRef> {
        fields
            .get(ATTACHMENTS_KEY)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    };
    if let Some((row, index)) = at {
        let fields = publisher.entity(super::rows::ROWS, row).filter(user_row)?;
        return shown(fields)
            .into_iter()
            .nth(index)
            .filter(|attachment| attachment.is(reference))
            .map(|attachment| attachment.mime);
    }
    publisher
        .entities(super::rows::ROWS)
        .filter(user_row)
        .flat_map(shown)
        .find(|attachment| {
            attachment.is(reference)
                && PREVIEWABLE
                    .iter()
                    .any(|kind| attachment.mime.to_ascii_lowercase().starts_with(kind))
        })
        .map(|attachment| attachment.mime)
}

/// 行与队列项的引用（`InputQuote`，02 §11.6）。
pub(crate) fn quotes(quotes: &[Quote]) -> Value {
    json!(
        quotes
            .iter()
            .map(|quote| InputQuote {
                text: quote.text.clone(),
                path: quote.path.clone(),
                sender_name: quote.sender_name.clone(),
                sender_id: quote.sender_id.clone(),
                message_id: quote.message_id.clone(),
                sent_at: quote.sent_at_ms,
            })
            .collect::<Vec<_>>()
    )
}

/// 行的代发来源（`InputSource`，02 §11.6）。
fn source(relay: &Relay) -> Value {
    json!(match relay {
        Relay::Plugin { plugin, tool_call } => InputSource::PluginUi(InputSourcePluginUi {
            plugin_id: plugin.clone(),
            tool_call_id: tool_call.clone(),
        }),
        Relay::Page { path, title } => InputSource::GenUi(InputSourceGenUi {
            path: path.clone(),
            title: title.clone(),
        }),
    })
}

/// `input.send` 的正文（[`as_send`]）。
const TEXT_KEY: &str = "text";

/// 行与队列项上这几样事实的字段名（`userInput` / `queue` 项，02 §11.6、§12.12）。
pub(crate) const ATTACHMENTS_KEY: &str = "attachments";
pub(crate) const QUOTES_KEY: &str = "quotes";
const SOURCE_KEY: &str = "source";
const MODEL_SELECTION_KEY: &str = "modelSelection";
const MODE_KEY: &str = "mode";
const PLAN_ENABLED_KEY: &str = "planEnabled";

/// 队列项上这条输入自己的执行配置（K-Q2，protocol-v5.md §18.2）与输入行上这一轮用的配置（§64.2）：`input.send` 的同名字段，没给的项不写。
pub(crate) fn config_fields(fields: &mut Fields, config: &TurnConfig) {
    if let Some(selection) = &config.selection {
        fields.insert(
            MODEL_SELECTION_KEY.to_owned(),
            super::state::model_selection(selection),
        );
    }
    if let Some(mode) = config.mode {
        fields.insert(MODE_KEY.to_owned(), json!(mode.node_name()));
    }
    if let Some(plan) = config.plan {
        fields.insert(PLAN_ENABLED_KEY.to_owned(), json!(plan));
    }
}

/// 用户输入行加上这条输入的显示事实（输入记录，§15.3.3）：活会话与冷读同一个函数。没有引用、来源时不写这两个字段（契约里缺席 = 没有）。
/// `content` 是这条用户消息的块（本机附件的 `previewRef` 从它来，§45.4）。
pub(crate) fn decorate(fields: &mut Fields, record: &InputRecord, content: &[ContentBlock]) {
    fields.insert(
        ATTACHMENTS_KEY.to_owned(),
        shown(&record.files, &record.uploads, content),
    );
    if !record.quotes.is_empty() {
        fields.insert(QUOTES_KEY.to_owned(), quotes(&record.quotes));
    }
    if let Some(relay) = &record.relay {
        fields.insert(SOURCE_KEY.to_owned(), source(relay));
    }
    // 这一轮用的配置（§64.2）：编辑这条输入时界面按它载入工具栏；没带配置的输入（TUI 与 `-p`）没有
    config_fields(fields, &record.config);
}

#[cfg(test)]
#[path = "inputs_tests.rs"]
mod tests;
