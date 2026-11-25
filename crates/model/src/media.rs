//! 模型不支持图片或 PDF 时的降级（docs/specs/rust-cli/model.md §10.5；Node `projectMessagesForInputFormat` 与
//! `createUnsupportedModelInputMediaText`）。
//!
//! Provider 配置的 `inputFormat.supportsImage` 为假（[`ModelEndpoint::capabilities`]）时，发送前把 user 内容与
//! 工具结果里的图片换成 Node 的占位文字；`supportsPdf` 为假（[`ModelEndpoint::capabilities`]）时同样换掉工具结果里的
//! document 块（Read 整读 PDF 的结果，修复线 F24：在支持 PDF 的模型上读过、之后换了模型）。只改这次请求：会话历史
//! 不变，换回支持的模型后照常发送。三种协议共用，在按协议编码之前调用（transport 的 `resolve`；工作线 F93 删掉计数请求之前 count_tokens 另调一次）。

use rust_cli_kernel_api::message::{ContentBlock, ImageSource, MediaOrigin, ToolResultContent};
use rust_cli_kernel_api::model::ApiMessage;
use rust_cli_kernel_api::provider::ModelEndpoint;

/// Node 的占位文字：`[Attached <mime>]`（用户附上的带名字：`[Attached <mime>: <名字>]`，model.md §10.5）加一行说明；`kind` 是
/// Node 的 `image input` / `PDF input`。
fn placeholder(source: &ImageSource, label: Option<&str>, kind: &str) -> String {
    let (ImageSource::Base64 { media_type, .. } | ImageSource::Artifact { media_type, .. }) =
        source;
    let attached = match label {
        Some(label) => format!("[Attached {media_type}: {label}]"),
        None => format!("[Attached {media_type}]"),
    };
    format!(
        "{attached}\n[Media omitted from provider request because the selected model does not support {kind}.]"
    )
}

fn project_result(content: &mut [ToolResultContent], endpoint: &ModelEndpoint) {
    for item in content {
        let text = match item {
            ToolResultContent::Image { source } if !endpoint.capabilities.supports_image => {
                placeholder(source, None, "image input")
            }
            ToolResultContent::Document { source } if !endpoint.capabilities.supports_pdf => {
                placeholder(source, None, "PDF input")
            }
            _ => continue,
        };
        *item = ToolResultContent::Text { text };
    }
}

/// 端点不接受图片或 PDF 时，把请求消息里的图片与 document 块换成占位文字。
pub(crate) fn project(messages: &mut [ApiMessage], endpoint: &ModelEndpoint) {
    if endpoint.capabilities.supports_image && endpoint.capabilities.supports_pdf {
        return;
    }
    for message in messages {
        for content in &mut message.content {
            match &mut content.block {
                ContentBlock::Image { source, origin } if !endpoint.capabilities.supports_image => {
                    let label = origin.as_ref().and_then(MediaOrigin::image_label);
                    content.block = ContentBlock::Text {
                        text: placeholder(source, label, "image input"),
                    };
                }
                // 用户附上的 PDF（session-lifecycle.md §10.41）：同 tool_result 里的 document。
                ContentBlock::Document { source, origin }
                    if !endpoint.capabilities.supports_pdf =>
                {
                    let label = origin.as_ref().and_then(MediaOrigin::document_label);
                    content.block = ContentBlock::Text {
                        text: placeholder(source, label, "PDF input"),
                    };
                }
                ContentBlock::ToolResult { content, .. } => project_result(content, endpoint),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_cli_base::proto::mime;
    use rust_cli_base::secret::RedactedString;
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::model::{
        ApiContent, ApiRole, ModelRequest, ModelSelection, QuerySource, RequestOptions,
    };
    use rust_cli_kernel_api::provider::{
        Credential, EndpointAccess, ModelCapabilities, WireProtocol,
    };

    use super::*;

    fn endpoint(supports_image: bool) -> ModelEndpoint {
        ModelEndpoint {
            protocol: WireProtocol::OpenAiChatCompletions,
            base_url: "https://api.example.com/v1".to_owned(),
            model: "text-only".to_owned(),
            credential: Credential::ApiKey(RedactedString::new("sk-test")),
            headers: Vec::new(),
            body_patches: Vec::new(),
            capabilities: ModelCapabilities {
                supports_image,
                ..ModelCapabilities::default()
            },
            renewable: false,
            binding: None,
            single_use: false,
            access: EndpointAccess::default(),
        }
    }

    fn image() -> ImageSource {
        ImageSource::Base64 {
            media_type: mime::PNG.to_owned(),
            data: "iVBORw0KGgo=".to_owned(),
        }
    }

    fn plain(block: ContentBlock) -> ApiContent {
        ApiContent {
            block,
            cache_control: None,
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            selection: ModelSelection::new("p".to_owned(), "text-only".to_owned()),
            system: Vec::new(),
            messages: vec![ApiMessage {
                role: ApiRole::User,
                content: vec![
                    plain(ContentBlock::ToolResult {
                        tool_use_id: ToolUseId::new("toolu_1"),
                        content: vec![
                            ToolResultContent::Text {
                                text: "An image:".to_owned(),
                            },
                            ToolResultContent::Image { source: image() },
                        ],
                        is_error: false,
                    }),
                    plain(ContentBlock::Text {
                        text: "look".to_owned(),
                    }),
                    plain(ContentBlock::Image {
                        source: image(),
                        origin: None,
                    }),
                ],
                output_config: None,
            }],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: 1024,
            stop_sequences: Vec::new(),
            thinking: None,
            temperature: None,
            betas: Vec::new(),
            source: QuerySource::MainThread,
            options: RequestOptions::default(),
        }
    }

    /// F24：不支持 PDF 的端点把 tool_result 里的 document 块换成 Node 的占位（`PDF input`）。
    #[test]
    fn documents_become_node_placeholders_for_models_without_pdf_input() {
        let document = ToolResultContent::Document {
            source: ImageSource::Base64 {
                media_type: mime::PDF.to_owned(),
                data: "JVBERi0=".to_owned(),
            },
        };
        let messages = || {
            vec![ApiMessage {
                role: ApiRole::User,
                content: vec![plain(ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new("toolu_1"),
                    content: vec![document.clone()],
                    is_error: false,
                })],
                output_config: None,
            }]
        };
        let mut projected = messages();
        // 支持图片、不支持 PDF：只换 document。
        project(&mut projected, &endpoint(true));
        let ContentBlock::ToolResult { content, .. } = &projected[0].content[0].block else {
            panic!("tool result");
        };
        assert_eq!(
            content,
            &vec![ToolResultContent::Text {
                text: "[Attached application/pdf]\n[Media omitted from provider request because the selected model does not support PDF input.]".to_owned()
            }]
        );
        let mut kept = messages();
        project(
            &mut kept,
            &ModelEndpoint {
                capabilities: ModelCapabilities {
                    supports_pdf: true,
                    ..endpoint(true).capabilities
                },
                ..endpoint(true)
            },
        );
        assert_eq!(kept, messages());
    }

    /// ATTACH-2：用户附上的 PDF（user 内容里的 document 块）在不收 PDF 的端点上换成同一段占位。
    #[test]
    fn user_documents_become_node_placeholders_for_models_without_pdf_input() {
        let messages = || {
            vec![ApiMessage {
                role: ApiRole::User,
                content: vec![plain(ContentBlock::Document {
                    source: ImageSource::Base64 {
                        media_type: mime::PDF.to_owned(),
                        data: "JVBERi0=".to_owned(),
                    },
                    origin: None,
                })],
                output_config: None,
            }]
        };
        let mut projected = messages();
        project(&mut projected, &endpoint(true));
        assert_eq!(
            projected[0].content[0].block,
            ContentBlock::Text {
                text: "[Attached application/pdf]\n[Media omitted from provider request because the selected model does not support PDF input.]".to_owned()
            }
        );
        let mut kept = messages();
        project(
            &mut kept,
            &ModelEndpoint {
                capabilities: ModelCapabilities {
                    supports_pdf: true,
                    ..endpoint(true).capabilities
                },
                ..endpoint(true)
            },
        );
        assert_eq!(kept, messages());
    }

    /// SMALL-4：用户附上的块带 origin 时占位带名字（Node 的 `[Attached <类型>: <名字>]`）：图片取界面给的路径、再取文件名，
    /// PDF 取文件名（model.md §10.5）。
    #[test]
    fn attached_media_name_their_file_in_the_placeholder() {
        let origin = |file_name: &str, path: Option<&str>| {
            Some(MediaOrigin {
                file_name: Some(file_name.to_owned()),
                path: path.map(str::to_owned),
            })
        };
        let mut messages = vec![ApiMessage {
            role: ApiRole::User,
            content: vec![
                plain(ContentBlock::Image {
                    source: image(),
                    origin: origin("shot.png", Some("/Users/dev/shot.png")),
                }),
                plain(ContentBlock::Image {
                    source: image(),
                    origin: origin("pasted.png", None),
                }),
                plain(ContentBlock::Document {
                    source: ImageSource::Base64 {
                        media_type: mime::PDF.to_owned(),
                        data: "JVBERi0=".to_owned(),
                    },
                    origin: origin("doc.pdf", Some("/Users/dev/doc.pdf")),
                }),
            ],
            output_config: None,
        }];
        project(&mut messages, &endpoint(false));
        let texts: Vec<&str> = messages[0]
            .content
            .iter()
            .map(|item| match &item.block {
                ContentBlock::Text { text } => text.lines().next().unwrap(),
                other => panic!("not a placeholder: {other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "[Attached image/png: /Users/dev/shot.png]",
                "[Attached image/png: pasted.png]",
                "[Attached application/pdf: doc.pdf]",
            ]
        );
    }

    #[test]
    fn images_become_node_placeholders_for_text_only_models() {
        let text = "[Attached image/png]\n[Media omitted from provider request because the selected model does not support image input.]";
        let mut projected = request();
        project(&mut projected.messages, &endpoint(false));
        let blocks: Vec<&ContentBlock> = projected.messages[0]
            .content
            .iter()
            .map(|item| &item.block)
            .collect();
        assert_eq!(
            blocks[0],
            &ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: vec![
                    ToolResultContent::Text {
                        text: "An image:".to_owned()
                    },
                    ToolResultContent::Text {
                        text: text.to_owned()
                    },
                ],
                is_error: false,
            }
        );
        assert_eq!(
            blocks[2],
            &ContentBlock::Text {
                text: text.to_owned()
            }
        );
        let mut kept = request();
        project(&mut kept.messages, &endpoint(true));
        assert_eq!(kept, request());
    }
}
