//! 互操作的第三十段：上传的图片（docs/specs/rust-cli/node-store.md §4.4.1，ATTACHMENTS）。
//!
//! `upload-write <storage-dir>`：照上传服务的写法放 Artifact（用途 attachment、`text/plain`、内容是 data URL），再写一条带它们的用户输入
//! （正文之后：本机附上的图片、上传的 PDF 的 document 块、原样的上传图片、开回合时缩放之后另写的 JPEG，ATTACH-2；块带 origin，SMALL-4；
//! 与它的输入记录同一批）与一条回答。输出会话 id、各 Artifact 的 URI 与 data URL。
use rust_cli_kernel_api::input_extras::{InputRecord, UploadedFile};
use rust_cli_kernel_api::message::MediaOrigin;
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, ArtifactStore};

use super::*;

/// 1×1 的 PNG（与 E2E 粘贴的同一张）。
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACklEQVR4AWMAAQAABQABNtCI3QAAAABJRU5ErkJggg==";
const PNG_BYTES: u64 = 67;
/// 本机附上的图片：界面给的路径（示例路径，不读这个文件）。
const LOCAL_PATH: &str = "/Users/dev/pic.png";

/// 开回合时缩放之后写的图片（ATTACH-2，node-store.md §4.4.2）与上传的 PDF：只看存放的形状，内容只要是 data URL。
const JPEG: &str = "/9j/4AAQSkZJRg==";
const PDF: &str = "JVBERi0xLjQK";

/// 放一个 Artifact（用途 attachment、`text/plain`、data URL 文字），交回引用、URI 与 data URL。
async fn put(
    store: &NodeStore,
    id: &SessionId,
    media_type: &str,
    data: &str,
) -> Result<(ArtifactRef, String, String)> {
    let data_url = rust_cli_kernel_api::data_url::base64(media_type, data);
    let artifact = store
        .put(
            id,
            ArtifactPurpose::Attachment,
            "text/plain".into(),
            data_url.clone().into_bytes(),
        )
        .await?;
    let uri = rust_cli_kernel_api::artifact_uri::uri(&artifact.session, &artifact.id);
    Ok((artifact, uri, data_url))
}

fn uploaded(id: &SessionId, artifact: &ArtifactRef, name: &str, media_type: &str) -> UploadedFile {
    UploadedFile {
        session: id.clone(),
        artifact: artifact.id.clone(),
        name: name.into(),
        media_type: media_type.into(),
        size: PNG_BYTES,
    }
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    // 原样的上传图片用上传的 Artifact；缩放过的另写一个（JPEG）；PDF 用上传的 Artifact（session-lifecycle.md §10.41）
    let (small, uri, data_url) = put(&store, &id, "image/png", PNG).await?;
    let (shot, _, _) = put(&store, &id, "image/png", PNG).await?;
    let (resized, jpeg_uri, jpeg_data_url) = put(&store, &id, "image/jpeg", JPEG).await?;
    let (pdf, pdf_uri, pdf_data_url) = put(&store, &id, "application/pdf", PDF).await?;
    // 本机附上的图片开回合时读出、写成 Artifact（tools 的 attachment_media）
    let (local, local_uri, local_data_url) = put(&store, &id, "image/png", PNG).await?;
    let (ask, answer) = (message_id(), message_id());
    let record = InputRecord {
        uploads: vec![
            uploaded(&id, &small, "small.png", "image/png"),
            uploaded(&id, &shot, "shot.png", "image/png"),
            uploaded(&id, &pdf, "doc.pdf", "application/pdf"),
        ],
        ..InputRecord::new(ask.clone())
    };
    let image = |media_type: &str, artifact, origin| ContentBlock::Image {
        source: ImageSource::Artifact {
            media_type: media_type.into(),
            artifact,
        },
        origin: Some(origin),
    };
    let named = |name: &str| MediaOrigin {
        file_name: Some(name.into()),
        path: None,
    };
    let facts = vec![
        Fact::MessageAppended {
            message: Message::User(UserMessage {
                id: ask.clone(),
                content: vec![
                    text("What is in the shot?"),
                    image(
                        "image/png",
                        local,
                        MediaOrigin {
                            file_name: Some("pic.png".into()),
                            path: Some(LOCAL_PATH.into()),
                        },
                    ),
                    ContentBlock::Document {
                        source: ImageSource::Artifact {
                            media_type: "application/pdf".into(),
                            artifact: pdf,
                        },
                        origin: Some(named("doc.pdf")),
                    },
                    image("image/png", small, named("small.png")),
                    image("image/jpeg", resized, named("shot.png")),
                ],
                origin: UserOrigin::Human,
            }),
        },
        record.fact(),
    ];
    store.commit(&id, batch(Some("cmd_upload"), facts)).await?;
    store
        .commit(
            &id,
            batch(
                None,
                vec![reply(&answer, vec![text("A dot.")], StopReason::EndTurn)],
            ),
        )
        .await?;
    print(
        &json!({"session": id, "ask": ask, "uri": uri, "dataUrl": data_url, "jpegUri": jpeg_uri,
        "jpegDataUrl": jpeg_data_url, "pdfUri": pdf_uri, "pdfDataUrl": pdf_data_url,
        "localUri": local_uri, "localDataUrl": local_data_url, "localPath": LOCAL_PATH}),
    )
}
