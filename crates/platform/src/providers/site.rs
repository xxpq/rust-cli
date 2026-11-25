//! 没有 provider 时的模型能力（显式覆盖的端点，docs/specs/rust-cli/model.md §6、platform.md §3.3，修复线 F23）。
//!
//! 只套内置配置的通用规则（modelRules → modelApiRules → providerSiteRules）；模板与 provider 精确规则要 provider，
//! 不适用。URL 同样先规范化（`resolve::site_config`）。

use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{ModelCapabilities, TokenLimits, WireProtocol};
use serde_json::Value;

use super::catalog::{OptionMaps, capabilities, output_cap};
use super::resolve::site_config;
use super::rules::{Rule, rules};

/// 编译好的内置通用规则。
#[derive(Default)]
pub struct SiteRules {
    rules: Vec<Rule>,
    /// 入口读到的 `ZCODE_MAX_OUTPUT_TOKENS`（同平台层的端点，cli-args.md §9.1）：补丁的输出上限也与它取小。
    max_output_tokens: Option<u64>,
}

impl SiteRules {
    /// `builtin` 是内置配置的 `config`（`Registry::builtin_config`）。
    pub(crate) fn new(builtin: &Value) -> Self {
        Self {
            rules: rules(&builtin["modelConfigRules"], false),
            max_output_tokens: None,
        }
    }

    pub(crate) fn with_max_output_tokens(mut self, limit: Option<u64>) -> Self {
        self.max_output_tokens = limit;
        self
    }

    /// 同一份规则给出的档位与两个 option map（MODEL-OPTIONS 的 MO-3，docs/specs/rust-cli/model-options.md §12.3）。
    pub fn option_maps(&self, protocol: WireProtocol, base_url: &str, model: &str) -> OptionMaps {
        OptionMaps::of(&self.effective(protocol, base_url, model)["optionSpecs"])
    }

    /// 显式覆盖的请求补丁（MO-3）：与平台层的模板同一个求值（档位、两个 option map、`maxOutputTokens.map` 的输入取
    /// `min(规则的 max, ZCODE_MAX_OUTPUT_TOKENS, 请求交来的上限)`）。档位不在规则里时报 `InvalidRequest`（同平台层）。
    pub fn patches(
        &self,
        protocol: WireProtocol,
        base_url: &str,
        (selection, output_limit): (&ModelSelection, Option<u64>),
    ) -> Result<Vec<Value>, ModelError> {
        self.option_maps(protocol, base_url, &selection.model)
            .patches(
                (&selection.provider, &selection.model),
                selection.reasoning.as_deref(),
                output_cap(self.max_output_tokens, output_limit),
            )
    }

    /// （协议，baseUrl，模型 ID）按内置规则得到的能力。
    pub fn capabilities(
        &self,
        protocol: WireProtocol,
        base_url: &str,
        model: &str,
    ) -> ModelCapabilities {
        capabilities(&self.effective(protocol, base_url, model))
    }

    /// 同一份规则给出的压缩上限（修复线 F61，compact.md §2）：`contextWindow` 与 `optionSpecs.maxOutputTokens.max`。
    /// 规则里没有窗口（例如内置配置读不了，规则为空）时答不出，压缩按卡片与默认窗口；没有 max 时输出上限为 None。
    pub fn limits(
        &self,
        protocol: WireProtocol,
        base_url: &str,
        model: &str,
    ) -> Option<TokenLimits> {
        let effective = self.effective(protocol, base_url, model);
        let context_window = effective["properties"]["contextWindow"]
            .as_u64()
            .filter(|window| *window > 0)?;
        Some(TokenLimits {
            context_window,
            max_output_tokens: effective["optionSpecs"]["maxOutputTokens"]["max"].as_u64(),
        })
    }

    fn effective(&self, protocol: WireProtocol, base_url: &str, model: &str) -> Value {
        let api_type = match protocol {
            WireProtocol::AnthropicMessages => "anthropic-messages",
            WireProtocol::OpenAiChatCompletions => "openai-chat-completions",
            WireProtocol::OpenAiResponses => "openai-responses",
        };
        site_config(&self.rules, api_type, base_url, model)
    }
}

#[cfg(test)]
#[path = "site_tests.rs"]
mod tests;
