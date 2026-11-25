//! 端点的来源与 URL 规则（docs/specs/rust-cli/model.md §1、§6）。
//!
//! 模型端口每次请求经 [`ModelEndpoints`] 取一次端点：平台层按 Provider registry 与账号给出；显式覆盖
//! （`ZCODE_MODEL_BASE_URL` / `ZCODE_API_KEY`，修复线 F42）与测试用这里的 [`FixedEndpoint`]。

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use std::sync::Arc;

use rust_cli_kernel_api::provider::{
    Credential, EndpointAccess, EndpointQuery, ModelCapabilities, ModelEndpoint, ModelEndpoints,
    WireProtocol,
};
use serde_json::Value;

/// 显式给出的 Anthropic 端点（入口从 `ZCODE_MODEL_BASE_URL` / `ZCODE_API_KEY` 构造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicEndpoint {
    pub base_url: String,
    pub api_key: RedactedString,
}

impl AnthropicEndpoint {
    /// 显式覆盖没给地址时的缺省：SDK 的写法，不带 `/v1`（同 `@anthropic-ai/sdk`）。
    pub const DEFAULT_BASE_URL: &'static str = rust_cli_base::hosts::ANTHROPIC_API;
}

impl From<AnthropicEndpoint> for ModelEndpoint {
    fn from(endpoint: AnthropicEndpoint) -> Self {
        Self {
            protocol: WireProtocol::AnthropicMessages,
            base_url: endpoint.base_url,
            model: String::new(),
            credential: Credential::ApiKey(endpoint.api_key),
            headers: Vec::new(),
            body_patches: Vec::new(),
            // 没有规则时（测试）按缺省的语义：对话中间的 system 消息原样发出，模型接受图片
            // 与 PDF。入口给了规则（[`FixedEndpoint::with_rules`]）时每次按模型改写。
            capabilities: ModelCapabilities {
                supports_mid_conversation_system: true,
                supports_image: true,
                supports_pdf: true,
                ..ModelCapabilities::default()
            },
            renewable: false,
            binding: None,
            single_use: false,
            // 显式覆盖的端点不来自 Provider 配置：没有接入方式（请求处理按 base URL 判定）
            access: EndpointAccess::default(),
        }
    }
}

/// 显式覆盖的规则（入口用内置配置的通用规则算，docs/specs/rust-cli/model.md §6、§9、§11；model-options.md §12.3）：
/// 按选择里的模型给能力、档位与请求补丁（`/model` 可以换模型）。
pub trait EndpointRules: Send + Sync {
    fn capabilities(&self, model: &str) -> ModelCapabilities;
    /// 可选的档位；答不出时 None。
    fn reasoning_levels(&self, model: &str) -> Option<Vec<String>>;
    /// 这次请求的补丁（`output_limit` 是查询交来的上限，[`EndpointQuery::output_limit`]）。
    fn patches(
        &self,
        selection: &ModelSelection,
        output_limit: Option<u64>,
    ) -> Result<Vec<Value>, ModelError>;
}

/// 固定端点：每次请求都给同一份，请求体的 `model` 取选择里的模型。
pub struct FixedEndpoint {
    template: ModelEndpoint,
    rules: Option<Arc<dyn EndpointRules>>,
}

impl FixedEndpoint {
    pub fn new(template: impl Into<ModelEndpoint>) -> Self {
        Self {
            template: template.into(),
            rules: None,
        }
    }

    /// 规则（修复线 F23 的能力；MO-3 起还有档位与补丁）：端点上的能力项与补丁、[`ModelEndpoints::capabilities`] 与
    /// [`ModelEndpoints::reasoning_levels`] 都按选择里的模型从它取。
    ///
    /// 修改（MO-3）：以前只有能力来源，显式覆盖一律没有补丁，没有卡片的模型（GLM 等）一个推理字段都不发。
    pub fn with_rules(mut self, rules: Arc<dyn EndpointRules>) -> Self {
        self.rules = Some(rules);
        self
    }
}

#[async_trait]
impl ModelEndpoints for FixedEndpoint {
    /// 凭据不会换：只看查询里的选择与上限。
    async fn endpoint(&self, query: EndpointQuery<'_>) -> Result<ModelEndpoint, ModelError> {
        let mut endpoint = self.template.clone();
        endpoint.model.clone_from(&query.selection.model);
        if let Some(rules) = &self.rules {
            endpoint.capabilities = rules.capabilities(&query.selection.model);
            endpoint.body_patches = rules.patches(query.selection, query.output_limit)?;
        }
        Ok(endpoint)
    }

    async fn capabilities(&self, selection: &ModelSelection) -> Option<ModelCapabilities> {
        self.rules
            .as_ref()
            .map(|rules| rules.capabilities(&selection.model))
    }

    async fn reasoning_levels(&self, selection: &ModelSelection) -> Option<Vec<String>> {
        self.rules
            .as_ref()
            .and_then(|rules| rules.reasoning_levels(&selection.model))
    }
}

/// 去掉首尾空白与末尾的 `/`。
fn trimmed(base_url: &str) -> &str {
    base_url.trim().trim_end_matches('/')
}

/// 基址补 `/v1`，已以 `/v1` 结尾时不再补（Node `normalizeAnthropicBaseURL`）。请求地址与入口查能力规则用的
/// 写法（Node 的 Anthropic provider 写带 `/v1` 的地址）都从这里来（F55：以前 cli 另写了一份同样的规则）。
pub fn anthropic_v1_base_url(base_url: &str) -> String {
    let base = trimmed(base_url);
    if base.to_ascii_lowercase().ends_with("/v1") {
        base.to_owned()
    } else {
        format!("{base}/v1")
    }
}

/// `{base}/v1/messages?beta=true`（[`anthropic_v1_base_url`]）。用 SDK 的 beta 接口时，路径
/// 带 `?beta=true`。
pub(crate) fn anthropic_messages_url(base_url: &str) -> String {
    format!("{}/messages?beta=true", anthropic_v1_base_url(base_url))
}

/// `{base}/chat/completions`（OpenAI SDK 的 `baseURL` 已含 `/v1`）。
pub(crate) fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", trimmed(base_url))
}

/// `{base}/responses`。
pub(crate) fn responses_url(base_url: &str) -> String {
    format!("{}/responses", trimmed(base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_the_sdk_rules() {
        assert_eq!(
            anthropic_messages_url("http://127.0.0.1:9/"),
            "http://127.0.0.1:9/v1/messages?beta=true"
        );
        let zai = rust_cli_base::hosts::ZAI_ANTHROPIC;
        assert_eq!(
            anthropic_messages_url(&format!(" {zai} ")),
            format!("{zai}/v1/messages?beta=true")
        );
        assert_eq!(
            anthropic_messages_url("https://gw.example.com/V1/"),
            "https://gw.example.com/V1/messages?beta=true"
        );
        assert_eq!(
            chat_completions_url("https://api.example.com/v4/"),
            "https://api.example.com/v4/chat/completions"
        );
        assert_eq!(
            responses_url("https://api.example.com/v1"),
            "https://api.example.com/v1/responses"
        );
    }
}
