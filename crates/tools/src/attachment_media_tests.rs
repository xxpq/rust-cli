//! 用户附上的媒体与上传的文件（docs/specs/rust-cli/session-lifecycle.md §10.41 的表）。

use std::io::Cursor;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::input_extras::{AttachedFile, PROMPT_ATTACHMENT_SOURCE, UploadedFile};
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, MediaOrigin};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef};
use rust_cli_kernel_api::testing::{MemoryArtifacts, MemoryEnvironment, ScriptedInteractions};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_kernel_api::user_input::AttachedMedia;

use super::*;
use crate::test_more::Ctx;

const PDF: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n";

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([9, 99, 199])));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn context() -> ToolUseContext {
    let environment = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/work"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    })
    .with_bytes("/Users/dev/shot.png", &png(64, 48))
    .with_bytes("/Users/dev/wide.png", &png(2400, 10))
    .with_bytes("/Users/dev/broken.jpg", b"not an image")
    .with_bytes("/Users/dev/huge.png", &vec![0; 20 * 1024 * 1024 + 1])
    .with_bytes("/Users/dev/doc.pdf", PDF)
    .with_bytes("/Users/dev/fake.pdf", b"not a pdf")
    .with_bytes("/Users/dev/clip.mp4", b"video")
    .with_file("/Users/dev/folder.png/inside.txt", "x");
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::new([])));
    ctx.set_env(Arc::new(environment));
    ToolUseContext {
        artifacts: Arc::new(MemoryArtifacts::default()),
        ..ctx.build()
    }
}

fn local(path: &str, media_type: &str) -> AttachedFile {
    AttachedFile {
        path: path.to_owned(),
        name: "shown".to_owned(),
        media_type: media_type.to_owned(),
        size: 1,
    }
}

/// 上传服务写的 Artifact：`data:<类型>;base64,<字节>`。
async fn stored(context: &ToolUseContext, media_type: &str, bytes: &[u8]) -> ArtifactRef {
    let text = format!("data:{media_type};base64,{}", STANDARD.encode(bytes));
    context
        .artifacts
        .put(
            &SessionId::new("sess_1"),
            ArtifactPurpose::Attachment,
            "text/plain".to_owned(),
            text.into_bytes(),
        )
        .await
        .unwrap()
}

fn upload(artifact: &ArtifactId, name: &str, media_type: &str) -> UploadedFile {
    UploadedFile {
        session: SessionId::new("sess_1"),
        artifact: artifact.clone(),
        name: name.to_owned(),
        media_type: media_type.to_owned(),
        size: 1,
    }
}

/// 说明文字的 attachment 的原文（来源都是 `prompt_attachment`）。
fn notes(media: &AttachedMedia) -> Vec<&str> {
    media
        .attachments
        .iter()
        .inspect(|attachment| assert_eq!(attachment.source, PROMPT_ATTACHMENT_SOURCE))
        .map(|attachment| attachment.text.as_str())
        .collect()
}

/// 块里 Artifact 的 data URL 的类型与解出的字节。
async fn artifact_bytes(context: &ToolUseContext, source: &ImageSource) -> (String, Vec<u8>) {
    let ImageSource::Artifact { artifact, .. } = source else {
        panic!("an artifact source: {source:?}");
    };
    let text = String::from_utf8(context.artifacts.get(artifact).await.unwrap()).unwrap();
    let parsed = rust_cli_kernel_api::data_url::parse(&text).unwrap();
    (
        parsed.media_type.to_owned(),
        STANDARD.decode(parsed.data).unwrap(),
    )
}

/// 原样的上传图片用上传的 Artifact；超尺寸的缩放后写一个新的（JPEG），原 Artifact 不动。
#[tokio::test]
async fn uploaded_images_are_kept_or_resized_into_a_new_artifact() {
    let context = context();
    let small = stored(&context, "image/png", &png(64, 48)).await;
    let wide = stored(&context, "image/png", &png(2400, 10)).await;
    let media = resolve(
        &[],
        &[
            upload(&small.id, "small.png", "image/png"),
            upload(&wide.id, "wide.png", "image/png"),
        ],
        &context,
    )
    .await;
    assert!(media.attachments.is_empty());
    let [
        ContentBlock::Image {
            source: kept,
            origin: kept_origin,
        },
        ContentBlock::Image {
            source: resized,
            origin: resized_origin,
        },
    ] = media.content.as_slice()
    else {
        panic!("two images: {:?}", media.content);
    };
    // 上传的块带上传时的名字（模型不收图片时的占位用它，session-lifecycle.md §10.47）
    let named = |name: &str| {
        Some(MediaOrigin {
            file_name: Some(name.to_owned()),
            path: None,
        })
    };
    assert_eq!(
        (kept_origin, resized_origin),
        (&named("small.png"), &named("wide.png"))
    );
    assert_eq!(
        kept,
        &ImageSource::Artifact {
            media_type: "image/png".to_owned(),
            artifact: small,
        }
    );
    let ImageSource::Artifact {
        media_type,
        artifact,
    } = resized
    else {
        panic!("an artifact");
    };
    assert_eq!(media_type, "image/jpeg");
    assert_ne!(artifact.id, wide.id);
    let (stored_type, bytes) = artifact_bytes(&context, resized).await;
    assert_eq!(stored_type, "image/jpeg");
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (2000, 8));
}

/// 解不开的「图片」（Node `attachment_image_resize_failed`）与坏的 PDF 换成 Node 的占位；视频是「不收视频」的占位；找不到的跳过。
/// 占位是 attachment，不进用户消息（用户消息里的文字块就是用户的正文）。
#[tokio::test]
async fn bad_uploads_become_node_placeholders() {
    let context = context();
    let zeros = stored(&context, "image/png", &[0; 32]).await;
    let fake = stored(&context, "application/pdf", b"not a pdf").await;
    let clip = stored(&context, "video/mp4", b"video").await;
    let media = resolve(
        &[],
        &[
            upload(&zeros.id, "shot.png", "image/png"),
            upload(&fake.id, "doc.pdf", "application/pdf"),
            upload(&clip.id, "clip.mp4", "video/mp4"),
            upload(
                &ArtifactId::new("tool-result-gone"),
                "gone.png",
                "image/png",
            ),
        ],
        &context,
    )
    .await;
    assert!(media.content.is_empty(), "{:?}", media.content);
    assert_eq!(
        notes(&media),
        [
            "[Attached image/*: shot.png]",
            "[Attached application/pdf: doc.pdf]",
            "[Attached video/mp4: clip.mp4]\n[Media omitted from provider request because the selected model does not support video input.]",
        ]
    );
}

/// 上传的 PDF 是用原 Artifact 的 document；上传的文字（64 KiB 以内）是提醒，更大的不给模型；上传的图片排在别的块之后。
#[tokio::test]
async fn uploads_are_documents_texts_and_images_last() {
    let context = context();
    let picture = stored(&context, "image/png", &png(8, 8)).await;
    let pdf = stored(&context, "application/pdf", PDF).await;
    let notes = stored(&context, "text/plain;charset=utf-8", b"E2E_NOTES").await;
    let large = stored(&context, "application/json", &vec![b'x'; 64 * 1024 + 1]).await;
    let media = resolve(
        &[],
        &[
            upload(&picture.id, "picture.png", "image/png"),
            upload(&pdf.id, "doc.pdf", "application/pdf"),
            upload(&notes.id, "notes.txt", "text/plain"),
            upload(&large.id, "large.json", "application/json"),
        ],
        &context,
    )
    .await;
    assert!(
        matches!(
            media.content.as_slice(),
            [ContentBlock::Document { source: ImageSource::Artifact { media_type, artifact }, .. }, ContentBlock::Image { .. }]
                if media_type == "application/pdf" && artifact.id == pdf.id
        ),
        "{:?}",
        media.content
    );
    let [attachment] = media.attachments.as_slice() else {
        panic!("one text: {:?}", media.attachments);
    };
    assert_eq!(attachment.source, PROMPT_ATTACHMENT_SOURCE);
    assert!(
        attachment
            .text
            .contains("Attached inline text: notes.txt\nE2E_NOTES\n"),
        "{}",
        attachment.text
    );
}

/// 本机的图片：读出、缩放、写成一个新的 Artifact；超过 20 MiB 给路径引用的三行；解不开、不是文件给占位（类型按扩展名）；
/// 不存在的跳过。
#[tokio::test]
async fn local_images_follow_node() {
    let context = context();
    let media = resolve(
        &[
            local("/Users/dev/shot.png", "image/png"),
            local("/Users/dev/wide.png", "image/png"),
            local("/Users/dev/huge.png", "image/png"),
            local("/Users/dev/broken.jpg", "image/jpeg"),
            local("/Users/dev/folder.png", "image/png"),
            local("/Users/dev/missing.png", "image/png"),
        ],
        &[],
        &context,
    )
    .await;
    let [
        ContentBlock::Image {
            source: shot,
            origin: shot_origin,
        },
        ContentBlock::Image { source: wide, .. },
    ] = media.content.as_slice()
    else {
        panic!("two images: {:?}", media.content);
    };
    // 本机的块带文件名与界面给的路径（预览按路径找这一轮的 Artifact，占位用路径）
    assert_eq!(
        shot_origin,
        &Some(MediaOrigin {
            file_name: Some("shot.png".to_owned()),
            path: Some("/Users/dev/shot.png".to_owned()),
        })
    );
    assert_eq!(
        artifact_bytes(&context, shot).await,
        ("image/png".to_owned(), png(64, 48))
    );
    assert_eq!(artifact_bytes(&context, wide).await.0, "image/jpeg");
    assert_eq!(
        notes(&media),
        [
            "Attached image/png: /Users/dev/huge.png\nThe file was sent by local path because the image is larger than the inline media budget.\nUse the available file reading tools if you need to inspect the file contents.",
            "[Attached image/jpeg: /Users/dev/broken.jpg]",
            "[Attached image/png: /Users/dev/folder.png]",
        ]
    );
}

/// 本机的 PDF 写成 Artifact 的 document；不是 PDF 的给占位；本机的视频是「不收视频」的占位。
#[tokio::test]
async fn local_pdfs_and_videos_follow_node() {
    let context = context();
    let media = resolve(
        &[
            local("/Users/dev/doc.pdf", "application/pdf"),
            local("/Users/dev/fake.pdf", "application/pdf"),
            local("/Users/dev/clip.mp4", "video/mp4"),
        ],
        &[],
        &context,
    )
    .await;
    let [ContentBlock::Document { source, origin }] = media.content.as_slice() else {
        panic!("one document: {:?}", media.content);
    };
    assert_eq!(
        origin.as_ref().and_then(MediaOrigin::document_label),
        Some("doc.pdf")
    );
    assert_eq!(
        artifact_bytes(&context, source).await,
        ("application/pdf".to_owned(), PDF.to_vec())
    );
    assert_eq!(
        notes(&media),
        [
            "[Attached application/pdf: /Users/dev/fake.pdf]",
            "[Attached video/mp4: /Users/dev/clip.mp4]\n[Media omitted from provider request because the selected model does not support video input.]",
        ]
    );
}
