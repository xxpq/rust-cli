//! 解析结果 → 模型目录与端点模板（docs/specs/rust-cli/platform.md §3.3）。
//!
//! 规则对齐旧 Rust CLI 的 `apps/zcode-cli-rust/crates/model/src/registry_rules.rs`（对齐 Node）：
//! - 可执行的模型都能解析端点；`visibility: hidden` 的 provider 不进目录，但仍能按选择解析；
//! - 推理档位是 `optionSpecs.reasoningLevel.values`；没指定档位时用最后一个（Node `toModelOption`）；
//! - 请求补丁 = reasoningLevel map(档位) + maxOutputTokens map(max)；
//! - 默认选择：Node `resolveInitialModelSelection`——个人配置的 `defaultModelSelection` 可选（档位有效）时用它，
//!   否则目录第一项、最后一个档位；
//! - Team Plan（工作线 F49，platform.md §9.3）：团队 provider 的模型按可用的团队项目展开，每个项目一组，目录项与
//!   选择带团队项目；按选择找端点时团队项目也要相同（选择没带时取这个模型的第一个团队项目）。

use std::collections::HashMap;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{
    CatalogConnection, CatalogModel, EndpointAccess, ModelCapabilities, TokenLimits, WireProtocol,
};
use serde_json::Value;

use super::resolve::Resolved;
use super::rules::{ids, text};
use crate::option_map::evaluate;

/// 一个模型怎样取凭据。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Access {
    /// Provider 配置里的 API key。
    ApiKey(RedactedString),
    /// 官方账号模型：每次请求向账号层取（`access.accountType` / `access.mode`）。
    Account { account_type: String, mode: String },
}

/// 一个可执行模型的端点模板（凭据之外的部分）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Template {
    pub provider: String,
    pub model: String,
    pub protocol: WireProtocol,
    pub base_url: String,
    pub headers: Vec<(String, RedactedString)>,
    pub access: Access,
    /// Provider 配置的 `access.type` 与 `access.mode` 原样（进端点，给入口注入的请求处理判定，model.md §5.4）。
    pub endpoint_access: EndpointAccess,
    /// 档位与两个 option map（生效配置的 `optionSpecs`）。
    pub options: OptionMaps,
    /// 生效配置的能力（修复线 F23，platform.md §3.3）：目录项与端点都从这一份写。
    pub capabilities: ModelCapabilities,
    pub hidden: bool,
    pub provider_name: String,
    /// 团队项目（团队 provider 的模型每个项目一份，工作线 F49）；其他 provider 为 None。
    pub connection: Option<CatalogConnection>,
    /// 密钥管理页（[`super::resolve::ProviderEntry::key_url`]）：Puffin `/usage` 说当前模型跑在哪个站点的密钥上
    /// （platform.md §11.2）。
    pub key_url: Option<String>,
}

/// 一个模型的档位与两个 option map（生效配置的 `optionSpecs`，platform.md §3.3）。平台层的模板与显式覆盖的内置规则共用这一份
/// 求值（docs/specs/rust-cli/model-options.md §12.3）。
#[derive(Debug, Clone, PartialEq)]
pub struct OptionMaps {
    /// `optionSpecs.reasoningLevel.values`，按配置顺序。
    pub(crate) levels: Vec<String>,
    reasoning_map: String,
    max_map: String,
    /// `optionSpecs.maxOutputTokens.max`（没给时 0）。
    pub(crate) max_output_tokens: u64,
}

impl OptionMaps {
    /// `specs` 是生效配置的 `optionSpecs`。
    pub(crate) fn of(specs: &Value) -> Self {
        Self {
            levels: ids(&specs["reasoningLevel"]["values"]),
            reasoning_map: text(&specs["reasoningLevel"], "map").to_owned(),
            max_map: text(&specs["maxOutputTokens"], "map").to_owned(),
            max_output_tokens: specs["maxOutputTokens"]["max"].as_u64().unwrap_or(0),
        }
    }

    /// 可选的档位。
    pub fn levels(&self) -> &[String] {
        &self.levels
    }

    /// 选择的档位（没指定时用最后一个）对应的请求补丁。`max_output` 是入口读到的 `ZCODE_MAX_OUTPUT_TOKENS`
    /// （cli-args.md §9.1，修复线 F42）与请求交来的上限（`RequestOptions.provider_max_output_tokens`，工作线 F95）的
    /// 较小值（[`output_cap`]）：`maxOutputTokens.map` 用它与配置的 max 的较小值（模型目录的上限是模型能力的上限，两者取小）；
    /// 都没有时用配置的 max（Node 主循环的基线）。`owner` 是报错里的 provider 与模型。
    pub fn patches(
        &self,
        (provider, model): (&str, &str),
        reasoning: Option<&str>,
        max_output: Option<u64>,
    ) -> Result<Vec<Value>, ModelError> {
        let level = match reasoning {
            Some(level) if self.levels.iter().any(|known| known == level) => level,
            Some(level) => {
                return Err(ModelError::InvalidRequest {
                    message: format!(
                        "reasoning level {level} is not offered by {provider}/{model}"
                    ),
                });
            }
            None => self.levels.last().map(String::as_str).unwrap_or_default(),
        };
        let invalid = |error: anyhow::Error| ModelError::InvalidRequest {
            message: format!("provider option map failed: {error:#}"),
        };
        // 推理字段只照配置（MODEL-OPTIONS 的 MO-1，model-options.md §4.2）：每个模型、每种协议都合并 reasoningLevel 的补丁。
        // 修 bug：F121 时 Anthropic 协议的 GLM 不合并它（推理字段由借来的 opus 卡片写），选「关闭」不关、选 `enabled` 发
        // `effort: medium`，自建网关上一个推理字段都不发（model-options.md §3.3 D1、D2）。
        let reasoning = evaluate(&self.reasoning_map, "reasoningLevel", &Value::from(level))
            .map_err(invalid)?;
        let max_output_tokens = max_output.map_or(self.max_output_tokens, |cap| {
            cap.min(self.max_output_tokens)
        });
        let max = evaluate(
            &self.max_map,
            "maxOutputTokens",
            &Value::from(max_output_tokens),
        )
        .map_err(invalid)?;
        Ok(vec![reasoning, max])
    }
}

/// 交给 `maxOutputTokens.map` 的上限：入口的 `ZCODE_MAX_OUTPUT_TOKENS` 与请求交来的上限的较小值，都没有时 None（用配置的 max）。
/// 平台层的端点与显式覆盖的内置规则共用。
pub(crate) fn output_cap(env: Option<u64>, request: Option<u64>) -> Option<u64> {
    [env, request].into_iter().flatten().min()
}

impl Template {
    /// 选择的档位对应的请求补丁（[`OptionMaps::patches`]）。
    pub fn patches(
        &self,
        reasoning: Option<&str>,
        max_output: Option<u64>,
    ) -> Result<Vec<Value>, ModelError> {
        self.options
            .patches((&self.provider, &self.model), reasoning, max_output)
    }

    pub fn catalog_entry(&self) -> CatalogModel {
        CatalogModel {
            provider: self.provider.clone(),
            provider_name: self.provider_name.clone(),
            model: self.model.clone(),
            protocol: self.protocol,
            reasoning_levels: self.options.levels.clone(),
            context_window: self.capabilities.context_window,
            max_output_tokens: self.options.max_output_tokens,
            capabilities: self.capabilities,
            connection: self.connection.clone(),
        }
    }
}

/// provider 列出的一个模型，关着的也在（Settings › Models 的家族页，platform.md §11.4）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListedModel {
    pub provider: String,
    pub model: String,
    /// 有效配置的 `enabled`。
    pub enabled: bool,
    pub context_window: Option<u64>,
}

/// 一次解析的结果：全部可执行模型的模板与默认选择；`listed` 是每个 provider 列出的全部模型（不论能不能用）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub templates: Vec<Template>,
    pub listed: Vec<ListedModel>,
    pub default: Option<ModelSelection>,
    /// 个人配置 `defaultModelSelection` 的模型 ID 原样（不看目录）：显式覆盖没给模型时用它（model.md §6，F42）。
    pub configured_model: Option<String>,
}

impl Snapshot {
    /// 选择对应的模板：provider 与模型相同；选择带团队项目时，平铺的团队模板要团队项目也相同，没带时取第一个（platform.md §9.4）。
    /// 不带团队项目的模板（外部账号层下团队不平铺，§12.2）不看选择里的团队项目：团队由宿主的当前连接定。
    pub fn find(&self, selection: &ModelSelection) -> Option<&Template> {
        let mut candidates = self.templates.iter().filter(|template| {
            template.provider == selection.provider && template.model == selection.model
        });
        match selection.connection.as_deref() {
            Some(wanted) => candidates.find(|template| {
                template
                    .connection
                    .as_ref()
                    .is_none_or(|connection| connection.id == *wanted)
            }),
            None => candidates.next(),
        }
    }

    /// 压缩用的上限（修复线 F61，platform.md §4）：provider 与模型相同的第一个模板（隐藏的也算）。不看团队项目：同一个
    /// 模型在各团队项目下的配置相同，项目过期时窗口也照旧。
    pub fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits> {
        let template = self.templates.iter().find(|template| {
            template.provider == selection.provider && template.model == selection.model
        })?;
        Some(TokenLimits {
            context_window: template.capabilities.context_window,
            max_output_tokens: Some(template.options.max_output_tokens),
        })
    }

    pub fn catalog(&self) -> Vec<CatalogModel> {
        self.templates
            .iter()
            .filter(|template| !template.hidden)
            .map(Template::catalog_entry)
            .collect()
    }
}

/// 生效配置 `properties` 里的能力（Node 的字段名，docs/specs/rust-cli/model.md §11）。可执行的模型属性齐全
/// （`model_complete`）；缺的布尔按假、窗口按 0（与原来逐项读取相同）。`supports_tool_search` 不是配置字段：工作线 F89
/// 起总为假（docs/specs/rust-cli/tools-more.md §3.2.2：所有端点都用本地 ToolSearch，暂时不用原生）。
///
/// 修改（MODEL-OPTIONS 的 MO-1，model-options.md §4.3、待确认 2）：对话中间的 system 照配置，GLM 不再例外。F121 时 GLM
/// 总为真（它借 opus 的卡片形状），自建网关上与 Node 不同（Node 降成提醒）。
pub(crate) fn capabilities(effective: &Value) -> ModelCapabilities {
    let properties = &effective["properties"];
    let input = &properties["inputFormat"];
    ModelCapabilities {
        context_window: properties["contextWindow"].as_u64().unwrap_or(0),
        supports_tool_call: properties["supportsToolCall"] == true,
        supports_native_web_search: properties["supportsNativeWebSearch"] == true,
        supports_mid_conversation_system: properties["supportsMidConversationSystem"] == true,
        supports_json_schema_output: properties["supportsJsonSchemaOutput"] == true,
        requires_mfjs_tool_schema: properties["requiresMfjsToolSchema"] == true,
        supports_image: input["supportsImage"] == true,
        supports_pdf: input["supportsPdf"] == true,
        supports_video: input["supportsVideo"] == true,
        supports_audio: input["supportsAudio"] == true,
        // 修改（工作线 F89，用户决定：暂时不用服务器原生的 ToolSearch，含 z.ai / bigmodel）：以前按端点算（F52：协议是
        // anthropic-messages 且 `rule_url::is_zhipu_endpoint`），这些端点用原生。恢复原生时在这里按端点重新算。
        supports_tool_search: false,
        // 工作线 F93：输出上限与模板的 `max_output_tokens`、压缩的 `TokenLimits` 读同一个字段（docs/specs/rust-cli/model.md §11）。
        max_output_tokens: effective["optionSpecs"]["maxOutputTokens"]["max"].as_u64(),
    }
}

fn access(config: &Value) -> Option<Access> {
    let access = &config["access"];
    if access["type"] == "zhipu-account" {
        return Some(Access::Account {
            account_type: text(access, "accountType").to_owned(),
            mode: text(access, "mode").to_owned(),
        });
    }
    access["apiKey"]
        .as_str()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(|key| Access::ApiKey(RedactedString::new(key)))
}

pub(crate) fn snapshot(
    resolved: &Resolved,
    connections: &HashMap<String, Vec<CatalogConnection>>,
) -> Snapshot {
    let listed = resolved
        .providers
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(|model| ListedModel {
                provider: provider.provider_id.clone(),
                model: model.model_id.clone(),
                enabled: model.effective_config["enabled"] == true,
                context_window: Some(capabilities(&model.effective_config).context_window)
                    .filter(|w| *w > 0),
            })
        })
        .collect();
    let mut templates = Vec::new();
    for provider in resolved.providers.iter().filter(|p| p.executable) {
        let config = &provider.effective_config;
        let Some(protocol) = WireProtocol::parse(text(&config["api"], "type")) else {
            continue;
        };
        let Some(access) = access(config) else {
            continue;
        };
        let endpoint_access = EndpointAccess {
            kind: text(&config["access"], "type").to_owned(),
            mode: text(&config["access"], "mode").to_owned(),
        };
        // header 值必须是字符串；有一个不是就跳过这个 provider（与 Node 的 schema 校验相同的结果）。
        let headers: Option<Vec<(String, RedactedString)>> = config["api"]["headers"]
            .as_object()
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, value)| {
                        value
                            .as_str()
                            .map(|value| (name.clone(), RedactedString::new(value)))
                    })
                    .collect()
            })
            .unwrap_or(Some(Vec::new()));
        let Some(headers) = headers else {
            continue;
        };
        let mut models = Vec::new();
        for model in provider.models.iter().filter(|m| m.executable) {
            let effective = &model.effective_config;
            let specs = &effective["optionSpecs"];
            let base_url = text(&config["api"], "baseUrl");
            models.push(Template {
                provider: provider.provider_id.clone(),
                model: model.model_id.clone(),
                protocol,
                base_url: base_url.to_owned(),
                headers: headers.clone(),
                access: access.clone(),
                endpoint_access: endpoint_access.clone(),
                options: OptionMaps::of(specs),
                capabilities: capabilities(effective),
                hidden: provider.hidden,
                provider_name: provider
                    .provider_name
                    .clone()
                    .unwrap_or_else(|| provider.provider_id.clone()),
                connection: None,
                key_url: provider.key_url.clone(),
            });
        }
        // 团队 provider：每个可用的团队项目一组（项目在外、模型在内）；没有可用项目时 overlay 已不授权。
        match connections.get(&provider.provider_id) {
            Some(teams) => {
                for team in teams {
                    templates.extend(models.iter().map(|template| Template {
                        connection: Some(team.clone()),
                        ..template.clone()
                    }));
                }
            }
            None => templates.extend(models),
        }
    }
    let default = default_selection(&templates, &resolved.default_selection);
    Snapshot {
        templates,
        listed,
        default,
        configured_model: configured_model(&resolved.default_selection),
    }
}

/// `defaultModelSelection.modelId`（去掉首尾空白、非空）：Node `saveConfiguredDefault` 写的字段。显式覆盖的端点不在
/// 目录里，所以不看 provider、档位与“可选”（Node `resolveInitialModelSelection` 的判断只对目录里的模型有意义）。
pub(crate) fn configured_model(configured: &Value) -> Option<String> {
    configured["modelId"]
        .as_str()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
}

/// Node `resolveInitialModelSelection`（docs/specs/rust-cli/workspace-trust.md §3.1）：配置的缺省“可选”时原样用它
/// ——provider 不隐藏、模型在目录里、`options.reasoningLevel` 有值且在模型的档位里；否则目录第一项（第一个非隐藏
/// provider 的第一个有档位的模型）、最后一个档位（Node `completeNewModelSelection`）。
///
/// L31 修正：之前缺省没有档位或档位无效时仍用这个模型（取最后一个档位），与 Node 不同——Node 判它不可选、回到
/// 目录第一项（登录写的 `{providerId, modelId}` 也是这样），Node CLI 与本工程的新会话会选到不同的模型。
fn default_selection(templates: &[Template], configured: &Value) -> Option<ModelSelection> {
    let visible = || templates.iter().filter(|template| !template.hidden);
    let selectable = visible().find_map(|template| {
        let level = configured["options"]["reasoningLevel"].as_str()?;
        (configured["providerId"] == template.provider.as_str()
            && configured["modelId"] == template.model.as_str()
            && template.options.levels.iter().any(|known| known == level))
        .then(|| (template, level.to_owned()))
    });
    let (template, reasoning) = match selectable {
        Some(found) => found,
        None => visible()
            .find_map(|template| Some((template, template.options.levels.last()?.clone())))?,
    };
    Some(ModelSelection {
        reasoning: Some(reasoning),
        // 指向团队 provider 时是这个模型的第一个团队项目（缺省不存团队项目，D-F49-3）。
        connection: template
            .connection
            .as_ref()
            .map(|connection| Box::new(connection.id.clone())),
        ..ModelSelection::new(template.provider.clone(), template.model.clone())
    })
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tool_search_tests;
