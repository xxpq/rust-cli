//! 用户附上的媒体与上传的文件给模型的样子（docs/specs/rust-cli/session-lifecycle.md §10.41：每一种的规则与 Node 的出处）。
//! 开回合时在回合任务里做：读主 env 的文件或会话的 Artifact，图片缩放（阻塞线程池，`rust_cli_base::blocking`），
//! 写出的 Artifact 在用户消息提交之前已原子写好。

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rust_cli_base::proto::mime;
use rust_cli_base::units::{KIB, MIB};
use rust_cli_kernel_api::env::{EnvPath, ReadFile};
use rust_cli_kernel_api::env_files::FileKind;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::input_extras::{AttachedFile, MediaKind, UploadedFile, attachment_note};
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, MediaOrigin};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_kernel_api::user_input::{AttachedMedia, InputAttachment};
use rust_cli_kernel_api::{data_url, env_path};
use rust_cli_prompts::input_material;

use crate::attachment_image::{self, Prepared};
use crate::common::stat;

/// 本机的图片与 PDF 读进来的上限（Node `INLINE_MEDIA_ATTACHMENT_MAX_BYTES`、`PDF_INPUT_MAX_BYTES`）：超了给路径引用的文字。
const MEDIA_MAX_BYTES: u64 = 20 * MIB as u64;
/// 上传的文字内联的上限（Node `INLINE_TEXT_ATTACHMENT_MAX_BYTES`）：更大的不给模型。
const INLINE_TEXT_MAX_BYTES: usize = 64 * KIB;
/// PDF 的文件头（Node `isPdfBytes`）。
const PDF_MAGIC: &[u8] = b"%PDF-";
/// 上传的图片读不出、解不开时占位里的类型（Node `resolvedPlaceholderAttachment` 没给类型时的 `image/*`）。
const ANY_IMAGE: &str = "image/*";
/// 视频的占位多一行（Node `createUnsupportedModelInputMediaText` 的 `video input`）：rust 没有视频块。
const VIDEO_OMITTED: &str = "[Media omitted from provider request because the selected model does not support video input.]";
/// 本机文件超过上限时的路径引用（Node `resolvedPathReferenceAttachment`）：第二行的原因与第三行。
const IMAGE_TOO_LARGE: &str = "the image is larger than the inline media budget";
const PDF_TOO_LARGE: &str = "the PDF is larger than the inline PDF input limit";
const PATH_REFERENCE_HINT: &str =
    "Use the available file reading tools if you need to inspect the file contents.";

/// 一个附件给模型的样子。
enum Resolved {
    /// 进用户消息的块：图片、document。
    Block(ContentBlock),
    /// 上传的图片：排在全部别的块之后（Node `buildUserMessageEntries` 的粘贴图片）。
    UploadedImage(ContentBlock),
    /// 随这条输入提交的 attachment：占位文字与上传的文字。用户消息里的文字块就是用户的正文（行、标题、落库都这么读），所以说明
    /// 文字不放进用户消息。
    Attachment(InputAttachment),
    Skipped,
}

/// 本机的媒体（已过 deny 规则）与上传的文件 → 块与 attachment，各自按给出的顺序；上传的图片排在最后。
pub(crate) async fn resolve(
    files: &[AttachedFile],
    uploads: &[UploadedFile],
    context: &ToolUseContext,
) -> AttachedMedia {
    let mut out = AttachedMedia::default();
    let mut images = Vec::new();
    let mut place = |resolved| match resolved {
        Resolved::Block(block) => out.content.push(block),
        Resolved::UploadedImage(block) => images.push(block),
        Resolved::Attachment(attachment) => out.attachments.push(attachment),
        Resolved::Skipped => {}
    };
    for file in files {
        if context.cancel.is_cancelled() {
            break;
        }
        place(local(file, context).await);
    }
    for upload in uploads {
        if context.cancel.is_cancelled() {
            break;
        }
        place(uploaded(upload, context).await);
    }
    out.content.extend(images);
    out
}

/// 一段说明文字（占位、路径引用）。
fn note(text: String) -> Resolved {
    Resolved::Attachment(attachment_note(text))
}

fn placeholder(media_type: &str, label: &str) -> Resolved {
    note(input_material::placeholder(media_type, label))
}

/// 视频：rust 没有视频块，换成 Node 给不收视频的模型的那段文字。
fn video(media_type: &str, label: &str) -> Resolved {
    note(format!(
        "{}\n{VIDEO_OMITTED}",
        input_material::placeholder(media_type, label)
    ))
}

/// 本机图片占位里的类型（Node `inferImageMimeFromPath`）：按扩展名，认不出的按 PNG。
fn image_type_by_path(path: &str) -> &'static str {
    match env_path::extension(path).as_str() {
        ".jpg" | ".jpeg" => mime::JPEG,
        ".gif" => mime::GIF,
        ".webp" => mime::WEBP,
        _ => mime::PNG,
    }
}

/// 缩放（CPU 工作，在阻塞线程池里）。
async fn prepared(bytes: Vec<u8>) -> Option<Prepared> {
    match rust_cli_base::blocking(move || attachment_image::prepare(bytes)).await {
        Ok(Ok(prepared)) => Some(prepared),
        Ok(Err(error)) => {
            tracing::debug!(event = "tools.attachment.image_refused", reason = ?error,
                "an attached image cannot be given to the model");
            None
        }
        Err(error) => {
            tracing::warn!(event = "tools.attachment.image_failed", error = %error,
                "preparing an attached image failed");
            None
        }
    }
}

/// 写一个 Artifact（data URL 文字，Node 的形状，node-store.md §4.4.2），交回它的引用。
async fn put(context: &ToolUseContext, media_type: &str, bytes: &[u8]) -> Option<ArtifactRef> {
    let text = data_url::base64(media_type, &STANDARD.encode(bytes));
    context
        .artifacts
        .put(
            &context.session,
            ArtifactPurpose::Attachment,
            mime::TEXT_PLAIN.to_owned(),
            text.into_bytes(),
        )
        .await
        .inspect_err(|error| {
            tracing::warn!(event = "tools.attachment.store_failed", error = %error,
                "an attached file could not be written to the session");
        })
        .ok()
}

fn image(media_type: &str, artifact: ArtifactRef, origin: MediaOrigin) -> ContentBlock {
    ContentBlock::Image {
        source: ImageSource::Artifact {
            media_type: media_type.to_owned(),
            artifact,
        },
        origin: Some(origin),
    }
}

fn document(artifact: ArtifactRef, origin: MediaOrigin) -> ContentBlock {
    ContentBlock::Document {
        source: ImageSource::Artifact {
            media_type: mime::PDF.to_owned(),
            artifact,
        },
        origin: Some(origin),
    }
}

/// 本机附上的文件是哪个（session-lifecycle.md §10.47）：路径的最后一段（Node `basename(absolutePath)`）与界面给的路径。
fn local_origin(path: &str) -> MediaOrigin {
    MediaOrigin {
        file_name: Some(PathStyle::infer(path).file_name(path)),
        path: Some(path.to_owned()),
    }
}

/// 上传的文件是哪个：上传时的名字。
fn upload_origin(name: &str) -> MediaOrigin {
    MediaOrigin {
        file_name: (!name.is_empty()).then(|| name.to_owned()),
        path: None,
    }
}

/// PDF 的字节对不对（Node `parseInlinePdfDataUrl` 与本机读的检查）：不空、不超过上限、`%PDF-` 开头。
fn is_pdf(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.len() as u64 <= MEDIA_MAX_BYTES && bytes.starts_with(PDF_MAGIC)
}

/// 一个本机的媒体文件（Node `resolveLocalMediaAttachment`）。不存在的跳过（同 `@` 提及，§10.14）。
async fn local(file: &AttachedFile, context: &ToolUseContext) -> Resolved {
    let Some(kind) = MediaKind::of(&file.media_type) else {
        return Resolved::Skipped;
    };
    let path = file.path.as_str();
    let Ok(Some(found)) = stat(context, path).await else {
        return Resolved::Skipped;
    };
    let media_type = match kind {
        MediaKind::Image => image_type_by_path(path),
        MediaKind::Pdf => mime::PDF,
        MediaKind::Video => return video(&file.media_type, path),
    };
    if found.kind != FileKind::File {
        return placeholder(media_type, path);
    }
    if found.size > MEDIA_MAX_BYTES {
        let reason = if kind == MediaKind::Pdf {
            PDF_TOO_LARGE
        } else {
            IMAGE_TOO_LARGE
        };
        return note(format!(
            "Attached {media_type}: {path}\nThe file was sent by local path because {reason}.\n{PATH_REFERENCE_HINT}"
        ));
    }
    let request = ReadFile {
        path: EnvPath::new(path),
    };
    let Ok(read) = context
        .machine
        .files()
        .read_file(&context.workspace, request)
        .await
    else {
        return placeholder(media_type, path);
    };
    let stored = if kind == MediaKind::Pdf {
        if !is_pdf(&read.bytes) {
            return placeholder(media_type, path);
        }
        put(context, mime::PDF, &read.bytes)
            .await
            .map(|artifact| document(artifact, local_origin(path)))
    } else {
        let Some(ready) = prepared(read.bytes).await else {
            return placeholder(media_type, path);
        };
        put(context, ready.media_type, &ready.bytes)
            .await
            .map(|artifact| image(ready.media_type, artifact, local_origin(path)))
    };
    stored.map_or_else(|| placeholder(media_type, path), Resolved::Block)
}

/// 上传的 Artifact 里的 data URL：类型与解出的字节。读不出、不是 base64 的 data URL 时为 None。
async fn upload_bytes(
    context: &ToolUseContext,
    artifact: &ArtifactRef,
) -> Option<(String, Vec<u8>)> {
    let text = String::from_utf8(context.artifacts.get(artifact).await.ok()?).ok()?;
    let parsed = data_url::parse(&text)?;
    let bytes = STANDARD.decode(parsed.data).ok()?;
    Some((parsed.media_type.to_owned(), bytes))
}

/// 一个上传的文件（Node `resolveTurnAttachment` 的 URI 一支）。Artifact 找不到的（排队期间被删了）跳过（§39.6）。
async fn uploaded(upload: &UploadedFile, context: &ToolUseContext) -> Resolved {
    let artifact = match context
        .artifacts
        .find(&upload.session, &upload.artifact)
        .await
    {
        Ok(Some(artifact)) => artifact,
        Ok(None) => {
            tracing::debug!(event = "tools.attachment.upload_missing", artifact = %upload.artifact,
                "an uploaded file is gone; it is not given to the model");
            return Resolved::Skipped;
        }
        Err(error) => {
            tracing::warn!(event = "tools.attachment.upload_unreadable", artifact = %upload.artifact,
                error = %error, "an uploaded file could not be looked up; it is not given to the model");
            return Resolved::Skipped;
        }
    };
    let name = upload.name.as_str();
    let kind = MediaKind::of(&upload.media_type);
    if kind == Some(MediaKind::Video) {
        return video(&upload.media_type, name);
    }
    let read = upload_bytes(context, &artifact).await;
    match kind {
        Some(MediaKind::Image) => {
            let Some((stored_type, bytes)) = read else {
                return placeholder(ANY_IMAGE, name);
            };
            let Some(ready) = prepared(bytes).await else {
                return placeholder(ANY_IMAGE, name);
            };
            // 原样且类型相同时就用上传的 Artifact；改了（或 data URL 的类型与字节不符）才写一个新的
            if !ready.changed && stored_type == ready.media_type {
                return Resolved::UploadedImage(image(
                    ready.media_type,
                    artifact,
                    upload_origin(name),
                ));
            }
            match put(context, ready.media_type, &ready.bytes).await {
                Some(stored) => {
                    Resolved::UploadedImage(image(ready.media_type, stored, upload_origin(name)))
                }
                None => placeholder(ANY_IMAGE, name),
            }
        }
        Some(MediaKind::Pdf) => match read {
            Some((stored_type, bytes))
                if MediaKind::of(&stored_type) == Some(MediaKind::Pdf) && is_pdf(&bytes) =>
            {
                Resolved::Block(document(artifact, upload_origin(name)))
            }
            _ => placeholder(mime::PDF, name),
        },
        // 别的（文字）：解出来不超过 64 KiB 的按 UTF-8 内联（坏字节换成替换字符，同 Node `Buffer.toString`）；更大的、读不出的不给模型
        _ => match read {
            Some((_, bytes)) if bytes.len() <= INLINE_TEXT_MAX_BYTES => {
                Resolved::Attachment(attachment_note(input_material::upload_text(
                    name,
                    &String::from_utf8_lossy(&bytes),
                )))
            }
            _ => Resolved::Skipped,
        },
    }
}

#[cfg(test)]
#[path = "attachment_media_tests.rs"]
mod tests;
