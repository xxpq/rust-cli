//! 平台能力：Provider registry、账号、设置、每次请求的鉴权（docs/specs/rust-cli/platform.md）。
//!
//! 对内核与模型适配器只暴露 kernel-api 的三个端口：
//!
//! - [`ModelDirectory`]：可用模型目录与默认选择（带版本的快照）；
//! - [`ModelEndpoints`]：按 `ModelSelection` 解析出的端点与凭据。官方账号模型每次请求都从
//!   `credentials.json` 取当前凭据（每请求鉴权；OAuth 登录换短期项目令牌）；
//! - [`ModelLimits`]：压缩用的窗口与输出上限，同步读已发布的快照（修复线 F61）。
//!
//! 另给入口登录与退出（[`Platform::begin_login`]、[`Platform::configure_api_key`]、[`Platform::logout`]）。
//!
//! Provider 配置的格式与语义复制自 Host（`apps/zcode-host/crates/providers`）与 Node 独立 CLI，读写同一份
//! `~/.zcode/v2`；文件经 `rust-cli-fs`（写入持与 Node 同形的目录锁），网络经 `rust-cli-net`，路径由
//! `StoragePaths` 解析，不读进程环境（入口读好后传入）。

mod account;
mod auth;
mod consts;
mod credentials;
#[cfg(test)]
mod external_tests;
#[cfg(test)]
mod fake_server_tests;
pub mod local;
mod login;
pub mod mcp_oauth;
mod option_map;
mod personal;
mod providers;
mod team;
#[cfg(test)]
mod test_support;
mod token;
/// 套餐用量（Puffin `/usage`，platform.md §11）。
pub mod usage;

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::external_accounts::ExternalAccounts;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{
    AccessDenial, Credential, EndpointQuery, HeaderRequest, HeaderSource, ModelCapabilities,
    ModelCatalog, ModelDirectory, ModelEndpoint, ModelEndpoints, ModelLimits, RejectedCredential,
    SuppliedAuth, TokenLimits, WireProtocol,
};
use rust_cli_net::Egress;
use rust_cli_net::header;
use rust_cli_paths::{StoragePath, StoragePaths};

/// 测试替身（内存里的系统凭据库，platform.md §13.8）。
#[cfg(any(test, feature = "testing"))]
pub use credentials::testing;
pub use credentials::{CredentialError, CredentialSecret, SystemKeyStore};
pub use login::{LoginError, LoginOutcome, PendingLogin, SetupOutcome};
/// Node 的平台名（定义在 base，F55）；入口经 platform 取用，路径不变。
pub use rust_cli_base::js::node_platform;
pub use token::{AccountOrigins, TokenError};

use crate::auth::AccountAuth;
use crate::credentials::CredentialStore;
use crate::local::Settings;
pub use crate::providers::catalog::ListedModel;
pub use crate::providers::catalog::OptionMaps;
use crate::providers::catalog::{Access, Template};
pub use crate::providers::site::SiteRules;
use crate::providers::{Plans, Registry};
use crate::token::ProjectTokens;

/// 账号服务要用的出口、时钟、地址与系统凭据库（入口组装）。
#[derive(Clone)]
pub struct AccountServices {
    pub egress: Arc<Egress>,
    pub clock: Arc<dyn Clock>,
    pub origins: AccountOrigins,
    /// 凭据密钥所在的系统凭据库（`enc:v2:os`，platform.md §13.8）。
    pub keys: Arc<dyn SystemKeyStore>,
}

/// 把模型与档位存成新会话的缺省（`/effort`，docs/specs/rust-cli/tui-manage.md §2.2）：写个人 Provider 配置
/// `provider_config.json` 的 `defaultModelSelection`（与 Node `saveConfiguredDefault` 同一个文件与字段）。
pub async fn save_default_model(
    paths: &StoragePaths,
    selection: &ModelSelection,
) -> Result<String, String> {
    login::save_default_model((&paths.provider_config(), &paths.cli_config()), selection).await
}

/// 平台层的组装。
pub struct Platform {
    registry: Registry,
    credentials: CredentialStore,
    auth: AccountAuth,
    settings: Settings,
    services: AccountServices,
    personal_config: StoragePath,
    /// 旧 CLI 用户文件 `~/.zcode/cli/config.json`：个人配置不存在时 Node 从它导入（修复线 F2）。
    legacy_config: StoragePath,
    /// `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：`maxOutputTokens.map` 取它与配置的较小值。
    max_output_tokens: Option<u64>,
    /// 注入的账号凭据来源（缺口 G8）：有时账号模型的凭据向它要，没有时自己算（TUI、`-p`）。
    header_source: Option<Arc<dyn HeaderSource>>,
}

impl Platform {
    /// `paths` 已按入口读到的 `ZCODE_DATA_BASE_DIR`、`ZCODE_BUILTIN_PROVIDER_CONFIG_FILE` 调整过；`secret` 是
    /// 凭据密钥的材料；`services` 给账号服务（项目令牌、登录）用。
    pub fn new(paths: &StoragePaths, secret: &CredentialSecret, services: AccountServices) -> Self {
        let credentials = CredentialStore::new(paths.credentials(), secret, services.keys.clone());
        // 项目令牌的缓存由个人订阅查询与请求期共用（F50）：取目录时换到的令牌，第一次请求直接用。
        let tokens = Arc::new(ProjectTokens::new(
            services.egress.clone(),
            services.clock.clone(),
            services.origins.clone(),
        ));
        let settings = Settings::new(paths.settings().clone());
        let plans = Plans {
            personal: Arc::new(personal::PersonalPlans::new(
                services.egress.clone(),
                services.origins.clone(),
                tokens.clone(),
                credentials.clone(),
            )),
            teams: Arc::new(team::TeamPlans::new(
                services.egress.clone(),
                services.origins.clone(),
            )),
        };
        Self {
            registry: Registry::new(
                paths.builtin_provider_config().cloned(),
                paths.provider_config(),
                credentials.clone(),
                plans,
            ),
            auth: AccountAuth::new(credentials.clone(), tokens),
            credentials,
            settings,
            services,
            personal_config: paths.provider_config(),
            legacy_config: paths.cli_config(),
            max_output_tokens: None,
            header_source: None,
        }
    }

    /// 个人 provider 配置只用内存里的这一份（serve 每一轮由握手给出，docs/specs/rust-cli/multi-tenant.md §6）：
    /// 不读、不写个人配置文件。
    #[must_use]
    pub fn with_personal_config(mut self, config: Vec<u8>) -> Self {
        self.registry = self.registry.with_inline_personal(config);
        self
    }

    /// 注入外部账号层（platform.md §12，App 的 v5 模式）：之后账号 provider 的权益、当前连接与模型白名单以它为准，
    /// 不读凭据、不查个人订阅与团队；团队不平铺。TUI 与 `-p` 不注入。
    #[must_use]
    pub fn with_external_accounts(mut self, accounts: ExternalAccounts) -> Self {
        self.registry = self.registry.with_external(accounts);
        self
    }

    /// 注入账号模型每请求鉴权的来源（缺口 G8，app-protocol.md §5.2）：之后账号模型的凭据与请求头都向它要；
    /// API key 的 provider 不变。
    #[must_use]
    pub fn with_header_source(mut self, source: Arc<dyn HeaderSource>) -> Self {
        self.header_source = Some(source);
        self
    }

    /// 入口读到的 `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：每个模型的 `maxOutputTokens.map` 用
    /// min(配置的 max, 它) 求值（model.md §9）。
    #[must_use]
    pub fn with_max_output_tokens(mut self, limit: Option<u32>) -> Self {
        self.max_output_tokens = limit.map(u64::from);
        self
    }

    /// 个人 Provider 配置 `defaultModelSelection` 的模型 ID（与 Node 同一个文件与字段，经同一次解析）：显式覆盖
    /// （`ZCODE_API_KEY`）没给模型时用它（model.md §6，用户决定 2026-10-20，修复线 F42）。
    pub async fn configured_default_model(&self) -> Option<String> {
        self.registry
            .current()
            .await
            .snapshot
            .configured_model
            .clone()
    }

    /// 预读一次凭据（platform.md §13.12）：v2 文件要系统凭据库时现在取密钥（结果进进程内缓存）。macOS 的 TUI 在启动画面之前
    /// 调用，让钥匙串的系统弹框出在终端里的提示之后。读的结果不在这里用：读不到、文件损坏时，之后真正读的地方照常报错。
    pub async fn preload_credentials(&self) {
        if let Err(error) = self.credentials.read().await {
            tracing::debug!(%error, "the credentials were not readable at startup");
        }
    }

    /// 凭据文件 `credentials.json` 的位置。
    pub fn credentials_file(&self) -> &StoragePath {
        self.credentials.file()
    }

    /// 远程 MCP 服务器的 OAuth（docs/specs/rust-cli/ext.md §3.12）：令牌与账号凭据在同一个文件里、同一份密钥。
    pub fn mcp_oauth(&self) -> mcp_oauth::McpOAuthClient {
        mcp_oauth::McpOAuthClient::new(
            self.credentials.clone(),
            self.services.egress.clone(),
            self.services.clock.clone(),
        )
    }

    /// 同 [`Self::mcp_oauth`]，凭据库是委托模式（platform.md §13.13）：同一个文件，每个值经 `cipher` 加解密（接 App 的入口：
    /// 系统凭据库由 Main 独占，ext.md §3.12.9）。
    pub fn mcp_oauth_delegated(
        &self,
        cipher: Arc<dyn rust_cli_kernel_api::credential_cipher::CredentialCipher>,
    ) -> mcp_oauth::McpOAuthClient {
        mcp_oauth::McpOAuthClient::new(
            CredentialStore::delegated(self.credentials.file().clone(), cipher),
            self.services.egress.clone(),
            self.services.clock.clone(),
        )
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// 把当前模型与档位存成新会话的缺省（`/effort`，Node `rememberCurrentModelSelection`，
    /// docs/specs/rust-cli/tui-manage.md §2.2）：写个人配置的 `defaultModelSelection`。返回文件路径。
    pub async fn save_default_model(&self, selection: &ModelSelection) -> Result<String, String> {
        login::save_default_model((&self.personal_config, &self.legacy_config), selection).await
    }

    /// 内置配置的通用规则（显式覆盖的端点按它算能力，model.md §6，修复线 F23）。内置配置读不了时为空规则
    /// （能力是缺省值）。
    pub async fn site_rules(&self) -> SiteRules {
        match self.registry.builtin_config().await {
            Ok(builtin) => SiteRules::new(&builtin).with_max_output_tokens(self.max_output_tokens),
            Err(_) => SiteRules::default(),
        }
    }

    /// 团队项目不可用的提示（已过期 / 未分配：App 的文案加管理链接，platform.md §9.2）；`-p` 选不出模型时用它代替
    /// 通用的报错。`provider` 是 `--model` 指的 provider（没给时看所有团队 provider）。
    pub async fn team_notice(&self, provider: Option<&str>) -> Option<String> {
        self.registry.team_notice(provider, None).await
    }

    /// 新会话默认使用的模型；没有可用 Provider 时为空。
    pub async fn default_selection(&self) -> Option<ModelSelection> {
        self.registry.current().await.snapshot.default.clone()
    }

    /// 这次请求的凭据、附加的请求头与凭据的绑定：账号模型有注入的来源时向它要（G8；被拒的凭据带上它的绑定，
    /// §5.3；这一轮执行覆盖的凭据引用原样带上，§12.2），否则自己算（没有附加的请求头与绑定）。
    async fn request_auth(
        &self,
        template: &Template,
        selection: &ModelSelection,
        rejected: Option<(&RedactedString, Option<&String>)>,
        sessions: (
            Option<&rust_cli_kernel_api::ids::SessionId>,
            Option<&rust_cli_kernel_api::ids::SessionId>,
        ),
        (denied, execution): (Option<&AccessDenial>, Option<&str>),
    ) -> Result<SuppliedAuth, ModelError> {
        let (session, root) = sessions;
        // 闲时、高速卡的凭据只在这一轮（platform.md §12.2）：向来源要套餐的凭据会拿到普通套餐的令牌、发到别的端点。没有这一轮的凭据引用时
        // 照 Node 的 `ModelRequestAuthMissing` 报稳定的鉴权失败，不问来源；有引用时带着它问来源（来源按它现取）。
        if let Access::Account { mode, .. } = &template.access
            && account::PER_TURN_MODES.contains(&mode.as_str())
            && execution.is_none()
        {
            return Err(ModelError::Authentication {
                message: format!(
                    "{}: {}/{} needs the credentials of this turn, which the request does not carry",
                    account::MODEL_REQUEST_AUTH_MISSING,
                    selection.provider,
                    selection.model
                ),
            });
        }
        if let (Access::Account { account_type, mode }, Some(source)) =
            (&template.access, &self.header_source)
        {
            return source
                .request_auth(HeaderRequest {
                    selection: selection.clone(),
                    account_type: account_type.clone(),
                    mode: mode.clone(),
                    session: session.cloned(),
                    root: root.cloned(),
                    rejected: rejected.map(|(key, binding)| RejectedCredential {
                        fingerprint: token::fingerprint(key.expose()),
                        binding: binding.cloned(),
                    }),
                    denied: denied.cloned(),
                    execution: execution.map(str::to_owned),
                })
                .await;
        }
        // 拒绝访问之后（§5.3）：自己算的凭据换不出别的，只有注入的来源能判定；调用方交出原来的拒绝
        if denied.is_some() {
            return Err(no_source_for_denial());
        }
        let key = self
            .credential(template, rejected.map(|(key, _)| key))
            .await?;
        Ok(SuppliedAuth {
            api_key: Some(key),
            ..SuppliedAuth::default()
        })
    }

    async fn credential(
        &self,
        template: &Template,
        rejected: Option<&RedactedString>,
    ) -> Result<RedactedString, ModelError> {
        match &template.access {
            Access::ApiKey(key) => Ok(key.clone()),
            Access::Account { account_type, mode } => {
                let team = template
                    .connection
                    .as_ref()
                    .map(|connection| &connection.id);
                self.auth
                    .credential((&template.provider, account_type, mode), team, rejected)
                    .await
            }
        }
    }
}

#[async_trait]
impl ModelDirectory for Platform {
    async fn catalog(&self) -> ModelCatalog {
        let current = self.registry.current().await;
        ModelCatalog {
            revision: current.revision,
            models: current.snapshot.catalog(),
            default: current.snapshot.default.clone(),
        }
    }
}

/// 压缩的窗口与输出上限（修复线 F61，platform.md §4）：只读已发布的快照，不重读文件；配置的变化由取目录、取端点时
/// 的重读带进来，最多晚一步。
impl ModelLimits for Platform {
    fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits> {
        self.registry.published().snapshot.limits(selection)
    }
}

#[async_trait]
impl ModelEndpoints for Platform {
    /// 修 bug（工作线 F95，platform.md §3.3、tools-more.md §5 表后）：以前 `maxOutputTokens` 补丁的输入只是配置的上限
    /// （与 `ZCODE_MAX_OUTPUT_TOKENS` 取小），目录模型上 WebSearch 旁路请求自己的 `min(4096, 上限)` 被改写成配置的上限
    /// （GLM-5.3-Flash 发 128000）；Node 的 option map 以请求自己的上限为输入，发 4096。现在请求交来的上限也参与取小。
    ///
    /// 缺口 G8：查询里的会话只给注入的凭据来源用；没有来源时不看它。
    async fn endpoint(&self, query: EndpointQuery<'_>) -> Result<ModelEndpoint, ModelError> {
        let EndpointQuery {
            selection,
            rejected,
            output_limit,
            session,
            root,
            denied,
            execution,
        } = query;
        // 拒绝访问之后再问（§5.3）：没有注入的来源时不读配置、不做 IO，直接失败（TUI、`-p` 的请求因此与以前相同）
        if denied.is_some() && self.header_source.is_none() {
            return Err(no_source_for_denial());
        }
        let current = self.registry.current().await;
        let Some(template) = current.snapshot.find(selection) else {
            // 这个团队项目已过期 / 未分配：照 App 的文案提示（platform.md §9.2）。
            let team = selection.connection.as_deref();
            if let Some(notice) = self
                .registry
                .team_notice(Some(&selection.provider), team)
                .await
            {
                return Err(ModelError::Authentication { message: notice });
            }
            return Err(ModelError::InvalidRequest {
                message: format!(
                    "model {}/{} is not available (check the provider configuration or log in)",
                    selection.provider, selection.model
                ),
            });
        };
        let max_output = providers::catalog::output_cap(self.max_output_tokens, output_limit);
        let body_patches = template.patches(selection.reasoning.as_deref(), max_output)?;
        let previous = rejected.and_then(|endpoint| match &endpoint.credential {
            Credential::ApiKey(previous) => Some((previous, endpoint.binding.as_ref())),
            Credential::None => None,
        });
        let SuppliedAuth {
            api_key: key,
            headers: supplied,
            binding,
            single_use,
        } = self
            .request_auth(
                template,
                selection,
                previous,
                (session, root),
                (denied, execution),
            )
            .await?;
        let previous = previous.map(|(key, _)| key);
        // 被拒的凭据重新读到的还是同一个（手工 key 不会自己变）：不再重试，直接报鉴权失败。
        if let (Some(previous), Some(key)) = (previous, &key)
            && previous.expose() == key.expose()
        {
            return Err(ModelError::Authentication {
                message: format!("the credential of {} was rejected", selection.provider),
            });
        }
        let mut headers = template.headers.clone();
        // 账号登录的 provider：配置里静态的鉴权头不发，凭据只来自每请求鉴权（Node `withoutModelAuthHeaders`，工作线
        // F64，model.md §2.4）。以前静态的 `Authorization` 会盖过账号的凭据。
        if matches!(template.access, Access::Account { .. }) {
            without_auth_headers(&mut headers);
        }
        // Node 的 Provider 适配器对 Anthropic 兼容网关同时发 Bearer；配置里显式给了 Authorization 时以配置为准。
        // 注入的来源给的请求头（G8）：同名时覆盖配置里的。
        for (name, value) in supplied {
            headers.retain(|(known, _)| !known.eq_ignore_ascii_case(&name));
            headers.push((name, value));
        }
        if let Some(key) = &key
            && template.protocol == WireProtocol::AnthropicMessages
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(header::AUTHORIZATION))
        {
            headers.insert(
                0,
                (
                    header::AUTHORIZATION.to_owned(),
                    header::bearer(key.expose()),
                ),
            );
        }
        Ok(ModelEndpoint {
            protocol: template.protocol,
            base_url: template.base_url.clone(),
            model: template.model.clone(),
            credential: key.map_or(Credential::None, Credential::ApiKey),
            headers,
            body_patches,
            capabilities: template.capabilities,
            renewable: matches!(template.access, Access::Account { .. }),
            binding,
            single_use,
            access: template.endpoint_access.clone(),
        })
    }

    /// 模板的能力（隐藏的 provider 也能答，与 `endpoint` 相同的查找），不取凭据（修复线 F23）。
    async fn capabilities(&self, selection: &ModelSelection) -> Option<ModelCapabilities> {
        let current = self.registry.current().await;
        current
            .snapshot
            .find(selection)
            .map(|template| template.capabilities)
    }

    /// 模板的档位（同一个查找；MODEL-OPTIONS 的 MO-2，model-options.md §12.2）：子代理对档与旁路请求取第一档用它。
    async fn reasoning_levels(&self, selection: &ModelSelection) -> Option<Vec<String>> {
        let current = self.registry.current().await;
        current
            .snapshot
            .find(selection)
            .map(|template| template.options.levels.clone())
    }
}

/// 去掉 `authorization` 与 `x-api-key`（名字不分大小写，Node `MODEL_AUTH_HEADER_NAMES`）。
/// 拒绝访问之后没有能问的来源（§5.3）：model 的 transport 不把这句交给用户，交出的是原来的拒绝。
fn no_source_for_denial() -> ModelError {
    ModelError::Authentication {
        message: "no credential source can answer an access denial".to_owned(),
    }
}

fn without_auth_headers(headers: &mut Vec<(String, RedactedString)>) {
    headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case(header::AUTHORIZATION)
            && !name.eq_ignore_ascii_case(header::X_API_KEY)
    });
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
