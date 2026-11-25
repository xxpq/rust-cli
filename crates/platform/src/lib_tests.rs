//! 平台层对内核暴露的两个端口（platform.md §4）：目录、默认选择与每请求的端点。

use rust_cli_base::{hosts, models};
use rust_cli_kernel_api::provider::EndpointAccess;
use serde_json::json;

use super::*;
use crate::test_support::write;

const PERSONAL: &str = include_str!("../tests/fixtures/personal.json");
const ZAI: &str = "account:zai-individual-coding-plan";
const IDENTITY: &str = "key-0123456789abcdef01234567";

fn secret() -> CredentialSecret {
    CredentialSecret {
        explicit: Some(RedactedString::new("test-secret")),
        explicit_file: None,
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

async fn platform(dir: &std::path::Path) -> (Platform, StoragePaths) {
    let paths = StoragePaths::resolve(&Default::default(), dir, dir);
    write(paths.provider_config().as_path(), PERSONAL).await;
    let services = crate::test_support::offline_services().await;
    (Platform::new(&paths, &secret(), services), paths)
}

fn selection(provider: &str, model: &str, reasoning: Option<&str>) -> ModelSelection {
    ModelSelection {
        reasoning: reasoning.map(str::to_owned),
        ..ModelSelection::new(provider.to_owned(), model.to_owned())
    }
}

#[tokio::test]
async fn the_catalog_and_default_come_from_the_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, _paths) = platform(dir.path()).await;
    let catalog = platform.catalog().await;
    assert_eq!(catalog.revision, 1);
    let names: Vec<String> = catalog
        .models
        .iter()
        .map(|model| format!("{}/{}", model.provider, model.model))
        .collect();
    assert_eq!(
        names,
        [
            format!("personal:responses/{}", models::GPT_5_4),
            format!("personal:gateway/{}", models::GLM_5),
            "personal:gateway/custom-chat".to_owned(),
            format!("personal:anthropic/{}", models::OPUS_5),
        ]
    );
    assert_eq!(
        platform.default_selection().await,
        Some(selection("personal:gateway", "custom-chat", Some("low")))
    );
}

/// serve（docs/specs/rust-cli/multi-tenant.md §6）：每一轮的个人 provider 配置由握手给出，只在内存里用。
/// 目录与缺省选择同读文件时一样；provider 配置文件既不读也不写。
#[tokio::test]
async fn an_inline_personal_configuration_replaces_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let services = crate::test_support::offline_services().await;
    let platform = Platform::new(&paths, &secret(), services)
        .with_personal_config(PERSONAL.as_bytes().to_vec());
    let catalog = platform.catalog().await;
    let names: Vec<String> = catalog
        .models
        .iter()
        .map(|model| format!("{}/{}", model.provider, model.model))
        .collect();
    assert_eq!(
        names,
        [
            format!("personal:responses/{}", models::GPT_5_4),
            format!("personal:gateway/{}", models::GLM_5),
            "personal:gateway/custom-chat".to_owned(),
            format!("personal:anthropic/{}", models::OPUS_5),
        ]
    );
    assert_eq!(
        platform.default_selection().await,
        Some(selection("personal:gateway", "custom-chat", Some("low")))
    );
    assert!(
        rust_cli_fs::read_optional(paths.provider_config().as_path())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn api_key_providers_resolve_with_headers_and_patches() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, _paths) = platform(dir.path()).await;
    let endpoint = platform
        .endpoint(EndpointQuery::new(&selection(
            "personal:gateway",
            "custom-chat",
            Some("high"),
        )))
        .await
        .unwrap();
    assert_eq!(endpoint.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(endpoint.base_url, "https://gateway.example.com/v1");
    assert_eq!(endpoint.model, "custom-chat");
    assert_eq!(
        endpoint.credential,
        Credential::ApiKey(RedactedString::new("sk-personal-fixture"))
    );
    assert_eq!(
        endpoint.headers,
        vec![("X-Team".to_owned(), RedactedString::new("dev"))]
    );
    assert_eq!(
        endpoint.body_patches[1],
        json!({"max_completion_tokens": 8192})
    );
    assert!(!endpoint.renewable && !endpoint.capabilities.supports_mid_conversation_system);
    // 接入方式原样进端点（model.md §5.4：入口注入的请求处理据它判定）。
    assert_eq!(
        endpoint.access,
        EndpointAccess {
            kind: "api-key".to_owned(),
            mode: String::new(),
        }
    );
    // `inputFormat.supportsImage` 为假：适配器把图片换成占位文字（model.md §10.5）。
    assert!(!endpoint.capabilities.supports_image);

    // Anthropic 协议的 provider 另带 Bearer（Node 的 Provider 适配器）；claude-opus-5 支持中途 system 消息。
    let opus = platform
        .endpoint(EndpointQuery::new(&selection(
            "personal:anthropic",
            models::OPUS_5,
            None,
        )))
        .await
        .unwrap();
    assert_eq!(
        opus.headers,
        vec![(
            "authorization".to_owned(),
            RedactedString::new("Bearer sk-ant-fixture")
        )]
    );
    assert!(opus.capabilities.supports_mid_conversation_system);
    assert!(opus.capabilities.supports_image);

    let missing = platform
        .endpoint(EndpointQuery::new(&selection(
            "personal:nokey",
            models::OPUS_5,
            None,
        )))
        .await;
    assert!(matches!(missing, Err(ModelError::InvalidRequest { .. })));
}

/// 档位查询（MODEL-OPTIONS 的 MO-2，model-options.md §12.2）：模板的档位按配置顺序，与目录项相同；不在快照里的模型答不出。
#[tokio::test]
async fn reasoning_levels_come_from_the_template() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, _paths) = platform(dir.path()).await;
    let catalog = platform.catalog().await;
    let gateway = selection("personal:gateway", models::GLM_5, None);
    let listed = catalog
        .models
        .iter()
        .find(|entry| entry.provider == gateway.provider && entry.model == gateway.model)
        .map(|entry| entry.reasoning_levels.clone());
    assert!(listed.as_ref().is_some_and(|levels| !levels.is_empty()));
    assert_eq!(platform.reasoning_levels(&gateway).await, listed);
    let gone = selection("personal:gateway", "gone", None);
    assert_eq!(platform.reasoning_levels(&gone).await, None);
}

/// 请求交来的输出上限进 `maxOutputTokens` 补丁的输入（工作线 F95，platform.md §3.3）：取配置的 max、
/// `ZCODE_MAX_OUTPUT_TOKENS` 与它三者的最小值；不给时照旧是配置的 max（custom-chat 是 8192）。
#[tokio::test]
async fn the_request_output_limit_feeds_the_max_output_tokens_patch() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, _paths) = platform(dir.path()).await;
    let chat = selection("personal:gateway", "custom-chat", Some("high"));
    let patch = |endpoint: ModelEndpoint| endpoint.body_patches[1].clone();
    let limited = |limit| {
        platform.endpoint(EndpointQuery {
            output_limit: limit,
            ..EndpointQuery::new(&chat)
        })
    };
    assert_eq!(
        patch(limited(Some(4096)).await.unwrap()),
        json!({"max_completion_tokens": 4096})
    );
    assert_eq!(
        patch(limited(Some(500_000)).await.unwrap()),
        json!({"max_completion_tokens": 8192})
    );
    assert_eq!(
        patch(limited(None).await.unwrap()),
        patch(platform.endpoint(EndpointQuery::new(&chat)).await.unwrap())
    );
    let capped = platform.with_max_output_tokens(Some(2000));
    let both = capped
        .endpoint(EndpointQuery {
            output_limit: Some(4096),
            ..EndpointQuery::new(&chat)
        })
        .await
        .unwrap();
    assert_eq!(patch(both), json!({"max_completion_tokens": 2000}));
}

#[tokio::test]
async fn account_models_take_the_credential_on_every_request() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, paths) = platform(dir.path()).await;
    let select = selection(ZAI, models::GLM_5_3_ACCOUNT, Some("high"));
    assert!(matches!(
        platform.endpoint(EndpointQuery::new(&select)).await,
        Err(ModelError::InvalidRequest { .. })
    ));
    let login = |key: &str| {
        json!({
            account::identity_key(ZAI): IDENTITY,
            account::source_key(ZAI): "manual",
            account::manual_key(ZAI, IDENTITY): key,
        })
        .to_string()
    };
    write(paths.credentials().as_path(), login("sk-first")).await;
    let first = platform
        .endpoint(EndpointQuery::new(&select))
        .await
        .unwrap();
    assert_eq!(
        first.credential,
        Credential::ApiKey(RedactedString::new("sk-first"))
    );
    assert!(first.renewable);
    assert_eq!(first.base_url, hosts::ZAI_ANTHROPIC);
    assert_eq!(
        first.access,
        EndpointAccess {
            kind: "zhipu-account".to_owned(),
            mode: "individual-coding-plan".to_owned(),
        }
    );
    // 同一个 key 被拒：不能续期。
    assert!(matches!(
        platform
            .endpoint(EndpointQuery {
                rejected: Some(&first),
                ..EndpointQuery::new(&select)
            })
            .await,
        Err(ModelError::Authentication { .. })
    ));
    // 凭据换了：下一次请求拿到新的。
    write(paths.credentials().as_path(), login("sk-second")).await;
    let renewed = platform
        .endpoint(EndpointQuery {
            rejected: Some(&first),
            ..EndpointQuery::new(&select)
        })
        .await
        .unwrap();
    assert_eq!(
        renewed.credential,
        Credential::ApiKey(RedactedString::new("sk-second"))
    );
    assert_eq!(
        platform.catalog().await.models[0].model,
        models::GLM_5_3_ACCOUNT,
        "account models come first"
    );
}

/// 缺口 G8（app-protocol.md §5.2）：注入的来源记下每次请求，给固定的 key 与一个请求头。
struct Supplied(std::sync::Mutex<Vec<rust_cli_kernel_api::provider::HeaderRequest>>);

#[async_trait]
impl rust_cli_kernel_api::provider::HeaderSource for Supplied {
    async fn request_auth(
        &self,
        request: rust_cli_kernel_api::provider::HeaderRequest,
    ) -> Result<rust_cli_kernel_api::provider::SuppliedAuth, ModelError> {
        let count = {
            let mut seen = self.0.lock().unwrap();
            seen.push(request);
            seen.len()
        };
        Ok(rust_cli_kernel_api::provider::SuppliedAuth {
            api_key: Some(RedactedString::new(format!("sk-host-{count}"))),
            headers: vec![("x-host-sign".to_owned(), RedactedString::new("signed"))],
            binding: Some(format!("scope-{count}")),
            single_use: false,
        })
    }
}

/// 有注入的来源时，账号模型的凭据与请求头都向它要（带上会话与被拒凭据的指纹）；API key 的 provider 不问它。
#[tokio::test]
async fn an_injected_header_source_supplies_account_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, paths) = platform(dir.path()).await;
    let source = Arc::new(Supplied(std::sync::Mutex::new(Vec::new())));
    let platform = platform.with_header_source(source.clone());
    write(
        paths.credentials().as_path(),
        json!({
            account::identity_key(ZAI): IDENTITY,
            account::source_key(ZAI): "manual",
            account::manual_key(ZAI, IDENTITY): "sk-own",
        })
        .to_string(),
    )
    .await;
    let select = selection(ZAI, models::GLM_5_3_ACCOUNT, Some("high"));
    let session = rust_cli_kernel_api::ids::SessionId::new("session_1");
    let root = rust_cli_kernel_api::ids::SessionId::new("session_root");
    let first = platform
        .endpoint(EndpointQuery {
            session: Some(&session),
            root: Some(&root),
            ..EndpointQuery::new(&select)
        })
        .await
        .unwrap();
    // 来源给的绑定跟着凭据进端点，平台层不解释它（platform.md §5.3）
    assert_eq!(first.binding.as_deref(), Some("scope-1"));
    assert_eq!(
        first.credential,
        Credential::ApiKey(RedactedString::new("sk-host-1"))
    );
    let header = |endpoint: &ModelEndpoint, name: &str| {
        endpoint
            .headers
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, value)| value.expose().to_owned())
    };
    assert_eq!(header(&first, "x-host-sign").as_deref(), Some("signed"));
    assert_eq!(
        header(&first, "authorization").as_deref(),
        Some("Bearer sk-host-1")
    );
    let renewed = platform
        .endpoint(EndpointQuery {
            rejected: Some(&first),
            session: Some(&session),
            ..EndpointQuery::new(&select)
        })
        .await
        .unwrap();
    assert_eq!(
        renewed.credential,
        Credential::ApiKey(RedactedString::new("sk-host-2"))
    );
    let seen = source.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].session, Some(session.clone()));
    assert_eq!(seen[0].root, Some(root.clone()));
    assert_eq!(seen[0].rejected, None);
    // 401 之后：被拒凭据的指纹与它的绑定一起带回来源
    assert_eq!(
        seen[1].rejected,
        Some(rust_cli_kernel_api::provider::RejectedCredential {
            fingerprint: token::fingerprint("sk-host-1"),
            binding: Some("scope-1".to_owned()),
        })
    );
    assert_eq!(renewed.binding.as_deref(), Some("scope-2"));
    assert!(!seen[0].account_type.is_empty() && !seen[0].mode.is_empty());
    // API key 的 provider 照旧用配置里的 key，不问来源。
    let personal = platform
        .endpoint(EndpointQuery {
            session: Some(&session),
            ..EndpointQuery::new(&selection("personal:gateway", "custom-chat", Some("high")))
        })
        .await
        .unwrap();
    assert!(matches!(personal.credential, Credential::ApiKey(_)));
    assert_eq!(source.0.lock().unwrap().len(), 2);
}

/// 拒绝访问之后再问（platform.md §5.3）：有来源时把事实转给它（不带被拒的凭据）；没有来源时直接失败，不读凭据文件。
#[tokio::test]
async fn an_access_denial_goes_to_the_source_and_fails_without_one() {
    let denied = rust_cli_kernel_api::provider::AccessDenial {
        status: Some(403),
        code: Some("1311".to_owned()),
    };
    let select = selection(ZAI, models::GLM_5_3_ACCOUNT, Some("high"));
    let dir = tempfile::tempdir().unwrap();
    // 没有来源、也没有凭据文件：读了文件会报没登录，这里先于一切 IO 失败
    let (own, _) = platform(dir.path()).await;
    let refused = own
        .endpoint(EndpointQuery {
            denied: Some(&denied),
            ..EndpointQuery::new(&select)
        })
        .await;
    assert!(
        matches!(&refused, Err(ModelError::Authentication { message }) if message.contains("access denial")),
        "{refused:?}"
    );
    let source = Arc::new(Supplied(std::sync::Mutex::new(Vec::new())));
    let (platform, paths) = platform(dir.path()).await;
    let platform = platform.with_header_source(source.clone());
    write(
        paths.credentials().as_path(),
        json!({
            account::identity_key(ZAI): IDENTITY,
            account::source_key(ZAI): "manual",
            account::manual_key(ZAI, IDENTITY): "sk-own",
        })
        .to_string(),
    )
    .await;
    let endpoint = platform
        .endpoint(EndpointQuery {
            denied: Some(&denied),
            ..EndpointQuery::new(&select)
        })
        .await
        .unwrap();
    assert_eq!(
        endpoint.credential,
        Credential::ApiKey(RedactedString::new("sk-host-1"))
    );
    let seen = source.0.lock().unwrap().clone();
    assert_eq!(seen[0].denied, Some(denied));
    assert_eq!(seen[0].rejected, None);
}

/// 修复线 F61（platform.md §4）：压缩的上限同步读已发布的快照——还没取过目录时答不出；取过之后是配置的
/// `contextWindow` 与 `maxOutputTokens.max`，不看团队项目；改了配置不重读，下一次取目录或端点之后才是新值。
#[tokio::test]
async fn limits_come_from_the_published_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, paths) = platform(dir.path()).await;
    let chat = selection("personal:gateway", "custom-chat", None);
    assert_eq!(platform.limits(&chat), None);
    platform.catalog().await;
    let expected = TokenLimits {
        context_window: 64_000,
        max_output_tokens: Some(8_192),
    };
    assert_eq!(platform.limits(&chat), Some(expected));
    // 内置规则 `.*glm-5.*`：200000 / 64000。
    let glm = platform.limits(&selection("personal:gateway", models::GLM_5, None));
    assert_eq!(
        glm,
        Some(TokenLimits {
            context_window: 200_000,
            max_output_tokens: Some(64_000),
        })
    );
    let mut teamed = chat.clone();
    teamed.connection = Some(Box::new(rust_cli_kernel_api::model::ModelConnection {
        product_id: "prod".to_owned(),
        organization_id: "org".to_owned(),
        project_id: "proj".to_owned(),
    }));
    assert_eq!(platform.limits(&teamed), Some(expected));
    assert_eq!(
        platform.limits(&selection("personal:gateway", "missing", None)),
        None
    );
    // 改配置：同步查询不重读；取端点（每个请求都取）之后是新值。
    let changed = PERSONAL.replace("\"contextWindow\": 64000", "\"contextWindow\": 100000");
    assert_ne!(changed, PERSONAL);
    write(paths.provider_config().as_path(), changed).await;
    assert_eq!(platform.limits(&chat), Some(expected));
    let _ = platform.endpoint(EndpointQuery::new(&chat)).await;
    assert_eq!(
        platform.limits(&chat).map(|limits| limits.context_window),
        Some(100_000)
    );
}

/// 工作线 F64（model.md §2.4）：账号登录的 provider 去掉配置里静态的 `authorization` 与 `x-api-key`（不分大小写），其余照发。
#[test]
fn account_providers_drop_static_auth_headers() {
    let header = |name: &str, value: &str| (name.to_owned(), RedactedString::new(value));
    let mut headers = vec![
        header("Authorization", "Bearer static"),
        header("X-API-KEY", "static"),
        header("X-Team", "dev"),
        header("x-api-key-id", "kept"),
    ];
    without_auth_headers(&mut headers);
    assert_eq!(
        headers,
        [header("X-Team", "dev"), header("x-api-key-id", "kept")]
    );
}
