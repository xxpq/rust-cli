//! 请求里的 Artifact 图片与 document 在发出之前换成 base64（docs/specs/rust-cli/session-lifecycle.md §10.40、§10.41；README §6 规则 10：大内容用引用传、
//! 在使用侧物化）。会话的消息与落库只带引用；模型适配器拿不到 Artifact 存储，见到没物化的引用会报错。每次请求现读，不缓存。

use rust_cli_kernel_api::data_url;
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, ToolResultContent};
use rust_cli_kernel_api::model::ApiMessage;
use rust_cli_kernel_api::store::{ArtifactRef, ArtifactStore};

/// 读出 Artifact 里的 data URL（上传服务写的形状，与 Node 相同）；读不出、不是 data URL 时为 None。
async fn read(artifacts: &dyn ArtifactStore, artifact: &ArtifactRef) -> Option<ImageSource> {
    let bytes = artifacts
        .get(artifact)
        .await
        .inspect_err(|error| {
            tracing::warn!(event = "query.artifact.unreadable", artifact = %artifact.id, error = %error,
                "an image artifact in the request could not be read");
        })
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let parsed = data_url::parse(&text)?;
    Some(ImageSource::Base64 {
        media_type: parsed.media_type.to_owned(),
        data: parsed.data.to_owned(),
    })
}

/// 物化一个来源（就地换成 base64）。不是 Artifact 引用时不动；读不出时交回替换整个块的占位文字。
async fn replace(artifacts: &dyn ArtifactStore, source: &mut ImageSource) -> Option<String> {
    let ImageSource::Artifact {
        media_type,
        artifact,
    } = &*source
    else {
        return None;
    };
    if let Some(base64) = read(artifacts, artifact).await {
        *source = base64;
        return None;
    }
    Some(rust_cli_prompts::input_material::placeholder(
        media_type,
        &rust_cli_kernel_api::artifact_uri::uri(&artifact.session, &artifact.id),
    ))
}

/// 把请求消息里（用户内容与工具结果）的 Artifact 图片与文档换成 base64；读不出的换成占位文字，请求照常发出（同一张图在之后的
/// 每一轮都在历史里，发一个读不出的引用会让每一轮都失败）。
pub(crate) async fn materialize(messages: &mut [ApiMessage], artifacts: &dyn ArtifactStore) {
    for message in messages {
        for content in &mut message.content {
            match &mut content.block {
                // 用户附上的 PDF 与图片同一种引用（session-lifecycle.md §10.41）。
                ContentBlock::Image { source, .. } | ContentBlock::Document { source, .. } => {
                    if let Some(text) = replace(artifacts, source).await {
                        content.block = ContentBlock::Text { text };
                    }
                }
                ContentBlock::ToolResult { content, .. } => {
                    for item in content {
                        let (ToolResultContent::Image { source }
                        | ToolResultContent::Document { source }) = item
                        else {
                            continue;
                        };
                        if let Some(text) = replace(artifacts, source).await {
                            *item = ToolResultContent::Text { text };
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
