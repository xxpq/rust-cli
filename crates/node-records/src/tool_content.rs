//! 工具结果内容 ↔ Node tool part 的 `state`（docs/specs/rust-cli/node-store.md §4.6）。
//!
//! 写入：`output` 是 Node `modelMessageContentToText` 的文本；图片照 Node
//! `persistToolResultMediaAttachments` 写成 `state.attachments` 里的 file part，块的顺序记在
//! `state.metadata.modelContentLayout`（Node 冷恢复用同一个字段重建媒体顺序）。内容恰好是一个文本块时
//! 不写 layout，与 Node 的纯文本结果相同。
//!
//! 读取：有合法 layout 时按它重建块；否则 completed 读 `output`，error 读 `metadata.modelContent`
//! （没有时读 `error`），与 Node `hydrateMessageHistoryFromSession` 相同。
use rust_cli_base::proto::mime;
use rust_cli_kernel_api::message::{
    ImageSource, MediaOrigin, ToolResultContent, tool_reference_text,
};
use rust_cli_kernel_api::store::ArtifactRef;
use serde_json::{Value, json};

use crate::artifacts;
use crate::records::RUST_CLI;

/// 写进 tool part 的结果内容。
pub struct Stored {
    pub output: String,
    pub layout: Option<Value>,
    pub attachments: Vec<Value>,
}

/// Node `modelMessageContentBlockToText` 的图片占位（没有 placeholder 名）。
fn placeholder(media_type: &str) -> String {
    format!("[Attached {media_type}]")
}

/// 一张图片 → Node `FilePart`（键序同 Node）。内联图片的 url 是 data URL（Node 没有 artifact store
/// 时的写法）；Artifact 引用的 url 是 `zcode-artifact://`，引用本身记在 `metadata.rustCli`。
pub fn file_part(id: String, owner: (&str, &str), source: &ImageSource) -> Value {
    part(id, owner, source, None)
}

/// 用户附上的媒体照 Node 的 file part 记下是哪个文件（docs/specs/rust-cli/node-store.md §4.4.2，session-lifecycle.md §10.47）：本机附上的
/// 写 `filename`、`source`、`metadata.originalUrl`；上传的 PDF 写 `filename`；上传的图片不写（Node 的上传图片 part 没有文件名）。
pub fn user_file_part(
    id: String,
    owner: (&str, &str),
    source: &ImageSource,
    origin: Option<&MediaOrigin>,
    document: bool,
) -> Value {
    let shown = origin.map(|origin| Shown {
        file_name: origin
            .file_name
            .as_deref()
            .filter(|_| origin.path.is_some() || document),
        path: origin.path.as_deref(),
    });
    part(id, owner, source, shown)
}

/// file part 里写的文件名与本机路径。
struct Shown<'a> {
    file_name: Option<&'a str>,
    path: Option<&'a str>,
}

/// 键的位置同 Node（`message-persistence.ts`：mime、filename、url、source、metadata）。
fn part(
    id: String,
    (session, message): (&str, &str),
    source: &ImageSource,
    shown: Option<Shown<'_>>,
) -> Value {
    let (file_name, path) = shown.map_or((None, None), |shown| (shown.file_name, shown.path));
    let mut out = json!({"id": id, "messageID": message, "sessionID": session, "type": "file"});
    let (media_type, url, mut metadata) = match source {
        ImageSource::Base64 { media_type, data } => (
            media_type,
            format!("data:{media_type};base64,{data}"),
            json!({"recoverability": "provider_ready", "storageKind": "inline"}),
        ),
        ImageSource::Artifact {
            media_type,
            artifact,
        } => {
            let uri = artifacts::uri(artifact);
            let metadata = json!({"artifactUri": uri, "storageKind": "artifact",
                "sizeBytes": artifact.size, RUST_CLI: {"artifact": artifact}});
            (media_type, uri, metadata)
        }
    };
    out["mime"] = media_type.as_str().into();
    if let Some(name) = file_name {
        out["filename"] = name.into();
    }
    out["url"] = url.into();
    if let Some(path) = path {
        // Node `FilePartSource.text`：界面给的原文与它在 JS 里的长度（UTF-16 单元）
        out["source"] = json!({"type": "file", "path": path,
            "text": {"value": path, "start": 0, "end": path.encode_utf16().count()}});
        metadata["originalUrl"] = path.into();
    }
    out["metadata"] = metadata;
    out
}

/// file part 记下的是哪个文件（[`user_file_part`] 的反面，Node 写的同样读）：`source.text.value` 是界面给的路径，`filename` 是文件名。
pub fn origin_of(part: &Value) -> Option<MediaOrigin> {
    let text = |value: &Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let path = (part["source"]["type"] == "file")
        .then(|| text(&part["source"]["text"]["value"]))
        .flatten();
    let file_name = text(&part["filename"]);
    (path.is_some() || file_name.is_some()).then_some(MediaOrigin { file_name, path })
}

pub fn store(
    content: &[ToolResultContent],
    owner: (&str, &str),
    mut part_id: impl FnMut() -> String,
) -> Stored {
    let texts: Vec<String> = content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => text.clone(),
            ToolResultContent::Image { source } | ToolResultContent::Document { source } => {
                placeholder(media_type(source))
            }
            // Node 没有 tool_reference：`output` 里是降级文本，Node 续写时按文本发给模型
            // （docs/specs/rust-cli/tools-more.md §3.6）。
            ToolResultContent::ToolReference { tool_name } => tool_reference_text(tool_name),
        })
        .filter(|text| !text.is_empty())
        .collect();
    let mut attachments = Vec::new();
    let layout = match content {
        [ToolResultContent::Text { .. }] => None,
        _ => Some(Value::Array(
            content
                .iter()
                .map(|block| match block {
                    ToolResultContent::Text { text } => json!({"type": "text", "text": text}),
                    // PDF 与图片同一写法（file part，mime 是 application/pdf；node-store.md §4.6，F24）。
                    ToolResultContent::Image { source }
                    | ToolResultContent::Document { source } => {
                        attachments.push(file_part(part_id(), owner, source));
                        json!({"type": "attachment", "attachmentIndex": attachments.len() - 1})
                    }
                    // Node 的 layout 解析器不认识这一项，会退回 `output`；本工程按 layout 还原。
                    ToolResultContent::ToolReference { tool_name } => {
                        json!({"type": "tool_reference", "tool_name": tool_name})
                    }
                })
                .collect(),
        )),
    };
    Stored {
        output: texts.join("\n\n"),
        layout,
        attachments,
    }
}

/// 媒体块在文本里的占位（Node `modelMessageContentBlockToText`，没有文件名时）。
pub fn attached(source: &ImageSource) -> String {
    placeholder(media_type(source))
}

fn media_type(source: &ImageSource) -> &str {
    match source {
        ImageSource::Base64 { media_type, .. } | ImageSource::Artifact { media_type, .. } => {
            media_type
        }
    }
}

/// Node `isPdfMime`：去掉 `;` 之后的参数，不分大小写。
fn is_pdf(mime: &str) -> bool {
    mime.split(';')
        .next()
        .is_some_and(|base| base.trim().eq_ignore_ascii_case(mime::PDF))
}

/// Node `filePartToContentBlock` 的图片与 PDF 分支，加上本工程的 Artifact 引用。读不出时是 Node 的
/// 占位文本（Node 写的 artifact 媒体要读文件才能还原，冷读取不读 artifact）。
///
/// 修 bug（F24）：以前只认图片，Read 整读 PDF 的 document 块（`application/pdf` 的 data URL）冷读取后变成
/// `[Attached application/pdf: data:…]` 文字，续写时模型看不到文档；现在与 Node `isPdfMime` 分支一样还原。
pub fn from_file_part(part: &Value) -> ToolResultContent {
    let mime = part["mime"].as_str().unwrap_or("");
    let media = |source: ImageSource| {
        if is_pdf(mime) {
            ToolResultContent::Document { source }
        } else {
            ToolResultContent::Image { source }
        }
    };
    let artifact =
        serde_json::from_value::<ArtifactRef>(part["metadata"][RUST_CLI]["artifact"].clone());
    if let Ok(artifact) = artifact {
        return media(ImageSource::Artifact {
            media_type: mime.to_owned(),
            artifact,
        });
    }
    let url = part["url"].as_str().unwrap_or("");
    let inline = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .and_then(|(header, data)| Some((header.strip_suffix(";base64")?, data)))
        .filter(|(_, data)| !data.is_empty());
    if let Some((_, data)) = inline
        && is_pdf(mime)
    {
        return media(ImageSource::Base64 {
            media_type: mime::PDF.to_owned(),
            data: data.to_owned(),
        });
    }
    if let Some((header, data)) = inline
        && (mime == "image/*" || mime.starts_with("image/"))
    {
        let media_type = if mime == "image/*" { header } else { mime };
        return ToolResultContent::Image {
            source: ImageSource::Base64 {
                media_type: media_type.to_owned(),
                data: data.to_owned(),
            },
        };
    }
    let label = part["filename"].as_str().unwrap_or(url);
    ToolResultContent::Text {
        text: format!("[Attached {mime}: {label}]"),
    }
}

/// Node `parsePersistedToolMediaLayout` 的条目；本工程的空结果写空数组，也接受。
fn layout(state: &Value) -> Option<Vec<ToolResultContent>> {
    let entries = state["metadata"]["modelContentLayout"].as_array()?;
    let attachments = state["attachments"].as_array();
    entries
        .iter()
        .map(|entry| match entry["type"].as_str()? {
            "text" => Some(ToolResultContent::Text {
                text: entry["text"].as_str()?.to_owned(),
            }),
            "attachment" => {
                let index = usize::try_from(entry["attachmentIndex"].as_u64()?).ok()?;
                attachments?.get(index).map(from_file_part)
            }
            "tool_reference" => Some(ToolResultContent::ToolReference {
                tool_name: entry["tool_name"].as_str()?.to_owned(),
            }),
            _ => None,
        })
        .collect()
}

/// 已结束的 tool part 的结果内容。
pub fn restore(state: &Value) -> Vec<ToolResultContent> {
    if let Some(content) = layout(state) {
        return content;
    }
    let text = if state["status"] == "error" {
        state["metadata"]["modelContent"]
            .as_str()
            .or_else(|| state["error"].as_str())
    } else {
        state["output"].as_str()
    };
    vec![ToolResultContent::Text {
        text: text.unwrap_or("").to_owned(),
    }]
}
