//! 模型端口一侧的前缀（docs/specs/rust-cli/request-prefix.md §6）：一段段拼起来的请求体与整段写的逐字节相同（含补丁改了
//! `messages`、beta 的判定）；按这次端点决定拼前缀、整段重写还是不交回。

use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, ToolResultContent};
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, CacheControl, ModelRequest, TurnOutputConfig,
};
use rust_cli_kernel_api::provider::{
    Credential, EndpointAccess, ModelCapabilities, ModelEndpoint, WireProtocol,
};
use rust_cli_kernel_api::request_prefix::{PlanRef, PrefixPlan, WirePrefix};
use serde_json::json;

use super::*;
use crate::anthropic::request::tests::profile;
use crate::wire::tests::request;

fn endpoint(
    protocol: WireProtocol,
    capabilities: ModelCapabilities,
    patches: Vec<serde_json::Value>,
) -> ModelEndpoint {
    ModelEndpoint {
        protocol,
        base_url: "http://127.0.0.1:9".to_owned(),
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        credential: Credential::ApiKey(RedactedString::new("test-key")),
        headers: Vec::new(),
        body_patches: patches,
        capabilities,
        renewable: false,
        binding: None,
        single_use: false,
        access: EndpointAccess::default(),
    }
}

fn capabilities(image: bool, pdf: bool, mid_system: bool) -> ModelCapabilities {
    ModelCapabilities {
        supports_image: image,
        supports_pdf: pdf,
        supports_mid_conversation_system: mid_system,
        ..ModelCapabilities::default()
    }
}

fn plain(block: ContentBlock) -> ApiContent {
    ApiContent {
        block,
        cache_control: None,
    }
}

fn text(value: &str) -> ApiContent {
    plain(ContentBlock::Text {
        text: value.to_owned(),
    })
}

fn base64(media_type: &str) -> ImageSource {
    ImageSource::Base64 {
        media_type: media_type.to_owned(),
        data: "iVBORw0KGgo=".to_owned(),
    }
}

/// 一段会话的请求消息：各种块（文字里有要转义的字符、图片、PDF、工具调用与结果、thinking）、对话中间的 system 与每轮 effort，
/// 最后一块带缓存断点。
fn messages(seed: usize, count: usize) -> Vec<ApiMessage> {
    let mut out = Vec::new();
    for index in 0..count {
        let kind = (seed * 7 + index * 13) % 6;
        let message = match kind {
            0 => ApiMessage {
                role: ApiRole::User,
                content: vec![
                    text(&format!("ask {index} \"quoted\" \u{2028} 中文 \n tab\t")),
                    plain(ContentBlock::Image {
                        source: base64("image/png"),
                        origin: None,
                    }),
                ],
                output_config: None,
            },
            1 => ApiMessage {
                role: ApiRole::Assistant,
                content: vec![
                    plain(ContentBlock::Thinking {
                        thinking: format!("think {index}"),
                        signature: Some("sig".to_owned()),
                    }),
                    text("ok"),
                    plain(ContentBlock::ToolUse {
                        id: ToolUseId::new(format!("t{index}")),
                        name: "Read".to_owned(),
                        input: json!({"file_path": "/Users/dev/a.rs", "limit": index, "nested": {"z": 1, "a": [1.5, null]}}),
                    }),
                ],
                output_config: None,
            },
            2 => ApiMessage {
                role: ApiRole::User,
                content: vec![plain(ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new(format!("t{}", index.saturating_sub(1))),
                    content: vec![
                        ToolResultContent::Text {
                            text: "x".repeat(index * 31),
                        },
                        ToolResultContent::Image {
                            source: base64("image/png"),
                        },
                        ToolResultContent::Document {
                            source: base64("application/pdf"),
                        },
                    ],
                    is_error: index % 2 == 0,
                })],
                output_config: None,
            },
            3 => ApiMessage {
                role: ApiRole::System,
                content: vec![text(&format!("reminder {index}"))],
                output_config: None,
            },
            4 => ApiMessage {
                role: ApiRole::System,
                content: Vec::new(),
                output_config: Some(TurnOutputConfig {
                    effort: Some("high".to_owned()),
                }),
            },
            _ => ApiMessage {
                role: ApiRole::User,
                content: vec![plain(ContentBlock::Document {
                    source: base64("application/pdf"),
                    origin: None,
                })],
                output_config: None,
            },
        };
        out.push(message);
    }
    if let Some(last) = out
        .iter_mut()
        .rev()
        .find_map(|message| message.content.last_mut())
    {
        last.cache_control = Some(CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: Some("global".to_owned()),
        });
    }
    out
}

/// 一段段编码：每一步的消息从上一步的前缀之后开始，前 `settles[i]` 条并入前缀；最后一步的请求体与 beta 与整段编码的相同。
#[test]
fn spliced_bodies_equal_whole_bodies_byte_for_byte() {
    let mut checked = 0;
    for first_party in [false, true] {
        let profile = profile(first_party);
        for patches in [
            Vec::new(),
            vec![json!({"temperature": 0.2})],
            vec![json!({"messages": null}), json!({"messages": []})],
        ] {
            let endpoint = endpoint(
                WireProtocol::AnthropicMessages,
                capabilities(true, true, true),
                patches,
            );
            for seed in 0..12 {
                let all = messages(seed, 24);
                let identity = crate::anthropic::wire_identity(&endpoint, profile);
                let mut wire: Option<WirePrefix> = None;
                let mut at = 0;
                for (step, settle) in [3, 0, 5, 1, 7, 2].into_iter().enumerate() {
                    let tail = all[at..].to_vec();
                    let whole = Anthropic
                        .encode(&endpoint, request(all.clone()), profile)
                        .expect("whole");
                    let splice = Splice {
                        wire: wire.as_ref(),
                        settle,
                        identity,
                    };
                    let (spliced, next) =
                        Anthropic::encode_with(&endpoint, request(tail), profile, Some(splice))
                            .expect("spliced");
                    assert_eq!(
                        spliced.body, whole.body,
                        "first party {first_party}, seed {seed}, step {step}"
                    );
                    assert_eq!(spliced.headers, whole.headers, "seed {seed}, step {step}");
                    let next = next.expect("next prefix");
                    assert_eq!(next.messages, at + settle);
                    assert_eq!(next.identity, identity);
                    at += settle;
                    wire = Some(next);
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 2 * 3 * 12 * 6);
}

/// 测试替身的计划：记下 `full` 被要过几次、交回的前缀。
struct FakePlan {
    wire: Option<WirePrefix>,
    settle: usize,
    whole: Vec<ApiMessage>,
    kept: Mutex<Option<WirePrefix>>,
    fulls: Mutex<usize>,
}

impl PrefixPlan for FakePlan {
    fn wire(&self) -> Option<&WirePrefix> {
        self.wire.as_ref()
    }

    fn settle(&self) -> usize {
        self.settle
    }

    fn full(&self) -> BoxFuture<'static, Vec<ApiMessage>> {
        *self.fulls.lock().expect("lock") += 1;
        let whole = self.whole.clone();
        async move { whole }.boxed()
    }

    fn keep(&self, prefix: WirePrefix) {
        *self.kept.lock().expect("lock") = Some(prefix);
    }
}

/// 前两条已经按身份 `identity` 编码成前缀的计划；这次的消息是其余的。
fn planned(all: &[ApiMessage], identity: u32) -> (Arc<FakePlan>, ModelRequest) {
    let profile = profile(false);
    let endpoint = endpoint(
        WireProtocol::AnthropicMessages,
        capabilities(true, true, true),
        Vec::new(),
    );
    let (_, wire) = Anthropic::encode_with(
        &endpoint,
        request(all[..2].to_vec()),
        profile,
        Some(Splice {
            wire: None,
            settle: 2,
            identity,
        }),
    )
    .expect("prefix");
    let plan = Arc::new(FakePlan {
        wire,
        settle: 1,
        whole: all.to_vec(),
        kept: Mutex::new(None),
        fulls: Mutex::new(0),
    });
    (plan, request(all[2..].to_vec()))
}

async fn fitted(
    plan: &Arc<FakePlan>,
    mut request: ModelRequest,
    endpoint: &ModelEndpoint,
) -> (Option<Fitted>, ModelRequest) {
    let fitted = fit(
        PlanRef(plan.clone()),
        &mut request,
        endpoint,
        profile(false),
    )
    .await;
    (fitted, request)
}

/// 身份相同：拼前缀，不要整段；交回的前缀多了这次的 `settle` 条。
#[tokio::test]
async fn a_matching_identity_splices_and_keeps_the_next_prefix() {
    let all = messages(1, 6);
    let endpoint = endpoint(
        WireProtocol::AnthropicMessages,
        capabilities(true, true, true),
        Vec::new(),
    );
    let identity = crate::anthropic::wire_identity(&endpoint, profile(false));
    let (plan, request) = planned(&all, identity);
    let (fitted, request) = fitted(&plan, request, &endpoint).await;
    let fitted = fitted.expect("splice");
    assert!(!fitted.rebuilt);
    let http = encode(fitted, &endpoint, request.clone(), profile(false))
        .await
        .expect("encode");
    let whole = Anthropic
        .encode(&endpoint, crate::wire::tests::request(all), profile(false))
        .expect("whole");
    assert_eq!(http.body, whole.body);
    assert_eq!(
        plan.kept
            .lock()
            .expect("lock")
            .as_ref()
            .map(|wire| wire.messages),
        Some(3)
    );
}

/// 端点的能力变了（不收图片）：身份不同，消息换成整段，按新的身份重写前缀（整段的前「旧前缀 + settle」条）。
#[tokio::test]
async fn a_changed_identity_rebuilds_from_the_whole_messages() {
    let all = messages(1, 6);
    let before = endpoint(
        WireProtocol::AnthropicMessages,
        capabilities(true, true, true),
        Vec::new(),
    );
    let (plan, request) = planned(
        &all,
        crate::anthropic::wire_identity(&before, profile(false)),
    );
    let after = endpoint(
        WireProtocol::AnthropicMessages,
        capabilities(false, true, true),
        Vec::new(),
    );
    let (fitted, request) = fitted(&plan, request, &after).await;
    let fitted = fitted.expect("rebuilt");
    assert!(fitted.rebuilt);
    assert_eq!(request.messages, all);
    assert_eq!(*plan.fulls.lock().expect("lock"), 1);
    let mut projected = request;
    crate::media::project(&mut projected.messages, &after);
    let http = encode(fitted, &after, projected.clone(), profile(false))
        .await
        .expect("encode");
    let whole = Anthropic
        .encode(&after, projected, profile(false))
        .expect("whole");
    assert_eq!(http.body, whole.body);
    let kept = plan.kept.lock().expect("lock").clone().expect("kept");
    assert_eq!(kept.messages, 3);
    assert_eq!(
        kept.identity,
        crate::anthropic::wire_identity(&after, profile(false))
    );
}

/// 别的协议与要降级 system 消息的端点：消息换成整段，不拼、不交回（下一步整段）。
#[tokio::test]
async fn other_protocols_and_system_folding_use_the_whole_messages() {
    let all = messages(3, 6);
    assert!(all.iter().any(|message| message.role == ApiRole::System));
    let anthropic = endpoint(
        WireProtocol::AnthropicMessages,
        capabilities(true, true, true),
        Vec::new(),
    );
    let identity = crate::anthropic::wire_identity(&anthropic, profile(false));
    for other in [
        endpoint(
            WireProtocol::OpenAiChatCompletions,
            capabilities(true, true, true),
            Vec::new(),
        ),
        endpoint(
            WireProtocol::AnthropicMessages,
            capabilities(true, true, false),
            Vec::new(),
        ),
    ] {
        let (plan, request) = planned(&all, identity);
        let (fitted, request) = fitted(&plan, request, &other).await;
        assert!(fitted.is_none());
        assert_eq!(request.messages, all);
        assert!(plan.kept.lock().expect("lock").is_none());
    }
}
