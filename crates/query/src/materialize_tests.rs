//! 请求里的 Artifact 图片在发出之前换成 base64（docs/specs/rust-cli/session-lifecycle.md §10.40）。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, ImageSource, Message, StopReason, ToolResultContent,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, ArtifactStore};
use rust_cli_kernel_api::testing::{MemoryArtifacts, ScriptedModel, text_response};

use crate::run::tests::{parts, request, scope};

const PNG: &str = "iVBORw0KGgo=";

/// 上传服务写的 Artifact：内容是 data URL 文字。
async fn upload(artifacts: &MemoryArtifacts, media_type: &str) -> ArtifactRef {
    artifacts
        .put(
            &SessionId::new("sess_1"),
            ArtifactPurpose::Attachment,
            "text/plain".to_owned(),
            format!("data:{media_type};base64,{PNG}").into_bytes(),
        )
        .await
        .unwrap()
}

fn artifact_image(artifact: ArtifactRef) -> ImageSource {
    ImageSource::Artifact {
        media_type: "image/png".to_owned(),
        artifact,
    }
}

fn base64(media_type: &str) -> ImageSource {
    ImageSource::Base64 {
        media_type: media_type.to_owned(),
        data: PNG.to_owned(),
    }
}

/// 跑一次，交回模型收到的第一个请求里第一条消息的块。
async fn sent(messages: Vec<Message>, artifacts: Arc<MemoryArtifacts>) -> Vec<ContentBlock> {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut parts = parts(model.clone(), Vec::new());
    parts.artifacts = artifacts;
    let mut request = request("unused");
    request.messages = messages;
    let (scope, _sink) = scope(parts, &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let requests = model.requests();
    requests[0].messages[0]
        .content
        .iter()
        .map(|content| content.block.clone())
        .collect()
}

fn user(content: Vec<ContentBlock>) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_user"),
        content,
        origin: UserOrigin::Human,
    })
}

/// 用户消息里的上传图片：请求里是 data URL 里的类型与数据，正文在前、图片在后不变。
#[tokio::test]
async fn an_uploaded_image_is_sent_as_base64() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    // 声明的类型与认出的格式不同时以 data URL 为准（上传服务写的是认出的格式）
    let artifact = upload(&artifacts, "image/jpeg").await;
    let blocks = sent(
        vec![user(vec![
            ContentBlock::Text {
                text: "look".to_owned(),
            },
            ContentBlock::Image {
                source: artifact_image(artifact),
                origin: None,
            },
        ])],
        artifacts,
    )
    .await;
    assert_eq!(
        blocks,
        vec![
            ContentBlock::Text {
                text: "look".to_owned()
            },
            ContentBlock::Image {
                source: base64("image/jpeg"),
                origin: None,
            },
        ]
    );
}

/// 读不出的 Artifact（被删了）：换成 Node 读不出附件时的占位文字，请求照常发出。
#[tokio::test]
async fn an_unreadable_image_becomes_the_node_placeholder() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let gone = ArtifactRef {
        session: SessionId::new("sess_1"),
        id: rust_cli_kernel_api::ids::ArtifactId::new("tool-result-gone"),
        purpose: ArtifactPurpose::Attachment,
        media_type: "text/plain".to_owned(),
        size: 10,
    };
    let blocks = sent(
        vec![user(vec![
            ContentBlock::Text {
                text: "look".to_owned(),
            },
            ContentBlock::Image {
                source: artifact_image(gone),
                origin: None,
            },
        ])],
        artifacts,
    )
    .await;
    assert_eq!(
        blocks[1],
        ContentBlock::Text {
            text: "[Attached image/png: zcode-artifact://sess_1/tool-result-gone]".to_owned()
        }
    );
}

/// 工具结果里的 Artifact 图片同样物化（同一种引用，同一条规则）。
#[tokio::test]
async fn an_artifact_image_in_a_tool_result_is_sent_as_base64() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let artifact = upload(&artifacts, "image/png").await;
    let assistant = MessageId::new("msg_assistant");
    let messages = vec![
        user(vec![ContentBlock::Text {
            text: "read it".to_owned(),
        }]),
        Message::Assistant(AssistantMessage {
            id: assistant.clone(),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".to_owned(),
                input: serde_json::json!({"file_path": "/work/a.png"}),
            }],
            model: "m".to_owned(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new("msg_result"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: vec![ToolResultContent::Image {
                    source: artifact_image(artifact),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult { assistant },
        }),
    ];
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut parts = parts(model.clone(), Vec::new());
    parts.artifacts = artifacts;
    let mut request = request("unused");
    request.messages = messages;
    let (scope, _sink) = scope(parts, &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let requests = model.requests();
    let results: Vec<_> = requests[0]
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match &content.block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![vec![ToolResultContent::Image {
            source: base64("image/png")
        }]]
    );
}

/// ATTACH-2：用户附上的 PDF（user 内容里的 document，session-lifecycle.md §10.41）同样物化成 base64。
#[tokio::test]
async fn an_attached_pdf_is_sent_as_base64() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let artifact = upload(&artifacts, "application/pdf").await;
    let blocks = sent(
        vec![user(vec![
            ContentBlock::Text {
                text: "read it".to_owned(),
            },
            ContentBlock::Document {
                source: ImageSource::Artifact {
                    media_type: "application/pdf".to_owned(),
                    artifact,
                },
                origin: None,
            },
        ])],
        artifacts,
    )
    .await;
    assert_eq!(
        blocks[1],
        ContentBlock::Document {
            source: base64("application/pdf"),
            origin: None,
        }
    );
}
