//! Anthropic Messages 协议（docs/specs/rust-cli/model.md §3）：URL、header 与四个部件的组装。

use rust_cli_kernel_api::model::{ModelError, ModelRequest};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint};
use rust_cli_net::{HeaderValue, HttpRequest, header};
use serde_json::Value;

use crate::endpoint::anthropic_messages_url;
use crate::errors::Vendor;
use crate::profiles::Profile;
use crate::protocol::{Protocol, StreamDecoder};
use crate::wire::HEADER_TIMEOUT;

pub(crate) mod betas;
mod output;
pub(crate) mod request;
mod shape;
pub(crate) mod stream;
mod system_fold;
mod whole;
mod write;

use stream::AnthropicStream;

/// SDK 的 `anthropic-version`。
pub(crate) const API_VERSION: &str = "2023-06-01";
/// Anthropic 的版本头与 beta 头（消息与计数两种请求都发，F55：以前各写一份）。
pub(crate) const VERSION_HEADER: &str = "anthropic-version";
pub(crate) const BETA_HEADER: &str = "anthropic-beta";

pub(crate) struct Anthropic;

/// 编码身份的各位（request-prefix.md §6）：会改变消息字节的请求方案与端点能力。
const IDENTITY_FIRST_PARTY: u32 = 1;
const IDENTITY_THIRD_PARTY_SYSTEM: u32 = 1 << 1;
const IDENTITY_IMAGE: u32 = 1 << 2;
const IDENTITY_PDF: u32 = 1 << 3;
const IDENTITY_MID_SYSTEM: u32 = 1 << 4;

/// 这次端点的消息编码身份（request-prefix.md §6）：身份相同，同一条消息写出的字节相同。看的是 `shape`（一方的专有项、
/// 缓存的 `scope`）、图片与 PDF 的占位（`media::project`）与对话中间的 system（`system_fold`）。
pub(crate) fn wire_identity(endpoint: &ModelEndpoint, profile: &Profile) -> u32 {
    let capabilities = &endpoint.capabilities;
    [
        (profile.first_party_extras, IDENTITY_FIRST_PARTY),
        (
            profile.system == crate::profiles::SystemShape::ThirdParty,
            IDENTITY_THIRD_PARTY_SYSTEM,
        ),
        (capabilities.supports_image, IDENTITY_IMAGE),
        (capabilities.supports_pdf, IDENTITY_PDF),
        (
            capabilities.supports_mid_conversation_system,
            IDENTITY_MID_SYSTEM,
        ),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(0, |bits, (_, bit)| bits | bit)
}

/// 这次请求的 system 消息要降级（端点不支持对话中间的 system，前缀或这次的消息里有 system 消息）：降级按顺序合并，
/// 这时不拼前缀（request-prefix.md §4 的表）。
pub(crate) fn folds_system(
    endpoint: &ModelEndpoint,
    request: &ModelRequest,
    prefix_flags: u32,
) -> bool {
    !endpoint.capabilities.supports_mid_conversation_system
        && (betas::MessageFlags::from_bits(prefix_flags).system
            || request
                .messages
                .iter()
                .any(|message| message.role == rust_cli_kernel_api::model::ApiRole::System))
}

impl Anthropic {
    /// 编码请求；`splice` 给出时拼上已编码的前缀，另交回下一步的前缀（request-prefix.md §6）。拼前缀时调用方已经排除了
    /// system 消息的降级（[`folds_system`]）。
    pub(crate) fn encode_with(
        endpoint: &ModelEndpoint,
        mut request: ModelRequest,
        profile: &Profile,
        splice: Option<request::Splice<'_>>,
    ) -> Result<
        (
            HttpRequest,
            Option<rust_cli_kernel_api::request_prefix::WirePrefix>,
        ),
        ModelError,
    > {
        shape::apply(&mut request, profile);
        if !endpoint.capabilities.supports_mid_conversation_system {
            system_fold::fold(&mut request);
        }
        request.selection.model.clone_from(&endpoint.model);
        let explicit = std::mem::take(&mut request.betas);
        let encoded =
            request::encode_spliced(request, profile, &endpoint.body_patches, &explicit, splice)?;
        // 兼容端点（方案不带一方的专有项）照 Node 发 beta（model.md §3.1 的“兼容端点”）。修 bug（F64）：以前从按特性的
        // 列表里只留两项，对话中间有 system 消息时少发 `mid-conversation-system`（Node 发）。
        let betas = if profile.first_party_extras {
            encoded.betas
        } else if crate::profiles::is_gateway(&endpoint.base_url) {
            // 用户决定（2026-10-23，F71）：经网关不发 `structured-outputs-2025-11-13`（转到 Bedrock 等后端的网关可能
            // 拒绝它），其余照 Node。
            let mut betas = encoded.node_betas;
            betas.retain(|beta| beta != betas::NODE_STRUCTURED_OUTPUTS);
            betas
        } else {
            encoded.node_betas
        };
        // SDK：`betas` 数组的 toString（逗号连接）进 `anthropic-beta`；没有 beta 时不发这个 header。
        let betas = betas.join(",");
        let mut http = HttpRequest::post(anthropic_messages_url(&endpoint.base_url), encoded.body)
            .header(header::ACCEPT, header::JSON)
            .header(header::CONTENT_TYPE, header::JSON)
            .header(VERSION_HEADER, API_VERSION);
        if !betas.is_empty() {
            http = http.header(BETA_HEADER, betas);
        }
        http.timeout = Some(HEADER_TIMEOUT);
        Ok((http, encoded.wire))
    }
}

impl Protocol for Anthropic {
    fn encode(
        &self,
        endpoint: &ModelEndpoint,
        request: ModelRequest,
        profile: &Profile,
    ) -> Result<HttpRequest, ModelError> {
        Self::encode_with(endpoint, request, profile, None).map(|(http, _)| http)
    }

    /// 只发 `x-api-key`（API key 的写法）。兼容网关需要的 Bearer 由平台层
    /// 写进 Provider header（Node 的 Provider 适配器两者都发）。
    fn credential_headers(&self, credential: &Credential) -> Vec<(String, HeaderValue)> {
        match credential {
            Credential::None => Vec::new(),
            Credential::ApiKey(key) => {
                vec![(
                    header::X_API_KEY.to_owned(),
                    HeaderValue::Secret(key.clone()),
                )]
            }
        }
    }

    fn decoder(&self, vendor: Vendor) -> Box<dyn StreamDecoder> {
        Box::new(AnthropicStream::new(vendor))
    }

    fn unstreamed(&self, request: &HttpRequest) -> Option<HttpRequest> {
        crate::whole::rewrite(request, whole::edit)
    }

    fn replay(&self, response: Value) -> String {
        whole::replay(response)
    }
}

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "protocol_parity_tests.rs"]
mod parity_tests;
