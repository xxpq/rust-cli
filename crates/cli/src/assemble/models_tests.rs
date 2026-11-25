use rust_cli_base::hosts::{ANTHROPIC_API, ANTHROPIC_API_V1, ZAI_ANTHROPIC};
use rust_cli_base::models::{GLM_5, GLM_5_3, GPT_5_4};
use rust_cli_kernel_api::provider::{CatalogModel, WireProtocol};

use super::*;

fn secret(value: &str) -> Option<RedactedString> {
    Some(RedactedString::new(value))
}

#[test]
fn the_anthropic_endpoint_comes_from_the_environment_snapshot() {
    assert_eq!(anthropic_endpoint(None, None), None);
    assert_eq!(
        anthropic_endpoint(Some("http://x".to_owned()), secret("")),
        None
    );
    assert_eq!(
        anthropic_endpoint(None, secret("sk-ant-1")),
        Some(AnthropicEndpoint {
            base_url: ANTHROPIC_API.to_owned(),
            api_key: RedactedString::new("sk-ant-1"),
        })
    );
    assert_eq!(
        anthropic_endpoint(Some("  ".to_owned()), secret("k")).map(|endpoint| endpoint.base_url),
        Some(ANTHROPIC_API.to_owned())
    );
    assert_eq!(
        anthropic_endpoint(Some(" http://127.0.0.1:9/proxy ".to_owned()), secret("k"))
            .map(|endpoint| endpoint.base_url),
        Some("http://127.0.0.1:9/proxy".to_owned())
    );
}

fn entry(provider: &str, model: &str) -> CatalogModel {
    CatalogModel {
        capabilities: Default::default(),
        provider: provider.to_owned(),
        provider_name: provider.to_owned(),
        model: model.to_owned(),
        protocol: WireProtocol::OpenAiChatCompletions,
        reasoning_levels: vec!["low".to_owned(), "high".to_owned()],
        context_window: 1,
        max_output_tokens: 1,
        connection: None,
    }
}

fn selection(provider: &str, model: &str, reasoning: Option<&str>) -> ModelSelection {
    ModelSelection {
        reasoning: reasoning.map(str::to_owned),
        ..ModelSelection::new(provider.to_owned(), model.to_owned())
    }
}

#[test]
fn the_model_flag_picks_from_the_catalog() {
    let catalog = ModelCatalog {
        revision: 1,
        models: vec![entry("p1", GLM_5), entry("p2", GLM_5), entry("p2", GPT_5_4)],
        default: Some(selection("p2", GPT_5_4, Some("low"))),
    };
    assert_eq!(
        pick(&catalog, None).unwrap(),
        selection("p2", GPT_5_4, Some("low"))
    );
    assert_eq!(
        pick(&catalog, Some(&format!("p2/{GLM_5}"))).unwrap(),
        selection("p2", GLM_5, None)
    );
    // 只给模型 ID 时取目录里第一个匹配。
    assert_eq!(
        pick(&catalog, Some(GLM_5)).unwrap(),
        selection("p1", GLM_5, None)
    );
    // 选中默认模型时沿用默认档位。
    assert_eq!(
        pick(&catalog, Some(GPT_5_4)).unwrap(),
        selection("p2", GPT_5_4, Some("low"))
    );
    let missing = pick(&catalog, Some(GLM_5_3)).unwrap_err();
    assert!(missing.contains(&format!("p2/{GPT_5_4}")), "{missing}");
    // 提示里是本工程读的名字（config.md §2.5），不提 Anthropic SDK 的变量。
    let none = pick(&ModelCatalog::default(), None).unwrap_err();
    assert!(none.contains("set ZCODE_API_KEY"), "{none}");
    assert!(!none.contains("ANTHROPIC"), "{none}");
}

/// 团队套餐的写法（cli-args.md §1.3，工作线 F49）：`[<团队>]` 选团队项目；多个团队时不默认取第一个；找不到时可用项
/// 的团队也写出来。
#[test]
fn the_model_flag_names_the_team() {
    use rust_cli_kernel_api::model::ModelConnection;
    use rust_cli_kernel_api::provider::CatalogConnection;
    let team = |project: &str, name: &str| {
        let mut entry = entry("t", "glm");
        entry.connection = Some(CatalogConnection {
            id: ModelConnection {
                product_id: "prod-1".to_owned(),
                organization_id: "org-t".to_owned(),
                project_id: project.to_owned(),
            },
            vendor: "BigModel".to_owned(),
            name: name.to_owned(),
        });
        entry
    };
    let catalog = ModelCatalog {
        revision: 1,
        models: vec![team("p-a", "A"), team("p-b", "B")],
        default: None,
    };
    let chosen = pick(&catalog, Some("t/glm[B]")).unwrap();
    assert_eq!(chosen.connection.unwrap().project_id, "p-b");
    assert_eq!(
        pick(&catalog, Some("t/glm")).unwrap_err(),
        "model t/glm matches several team plans; pass one of: t/glm[A], t/glm[B]"
    );
    assert_eq!(
        pick(&catalog, Some("x")).unwrap_err(),
        "model x is not configured (available: t/glm[A], t/glm[B])"
    );
    // 只有一个团队项目：不用写哪一个。
    let one = ModelCatalog {
        models: vec![team("p-a", "A")],
        ..catalog
    };
    assert_eq!(
        pick(&one, Some("t/glm"))
            .unwrap()
            .connection
            .unwrap()
            .project_id,
        "p-a"
    );
}

/// 显式覆盖的模型（model.md §6，用户决定 2026-10-20，修复线 F42）：`--model` → `ZCODE_MODEL` → 个人 Provider 配置的
/// `defaultModelSelection.modelId`；都没有时报错提示 `--model`，没有写死的缺省模型。
#[test]
fn the_explicit_model_falls_back_to_the_default_model_selection() {
    let pick = |flag: Option<&str>, env: Option<&str>, configured: Option<&str>| {
        explicit_model(
            flag.map(str::to_owned),
            env.map(str::to_owned),
            configured.map(str::to_owned),
        )
    };
    assert_eq!(pick(Some("a"), Some("b"), Some("c")).unwrap(), "a");
    assert_eq!(pick(None, Some("b"), Some("c")).unwrap(), "b");
    assert_eq!(pick(None, None, Some("c")).unwrap(), "c");
    assert_eq!(
        pick(None, None, None).unwrap_err(),
        "ZCODE_API_KEY is set: pass --model or set ZCODE_MODEL"
    );
}

#[test]
fn the_rule_url_keeps_the_old_v1_rule() {
    // F55：规则改由 model 的 `anthropic_v1_base_url` 给出；结果与原来 cli 自己的写法逐字相同。
    let old = |base_url: &str| {
        let base = base_url.trim().trim_end_matches('/');
        if base.to_ascii_lowercase().ends_with("/v1") {
            base.to_owned()
        } else {
            format!("{base}/v1")
        }
    };
    let with_slash = format!("{ANTHROPIC_API}/");
    for input in [
        ANTHROPIC_API,
        with_slash.as_str(),
        " https://gw.example.com/V1/ ",
        ZAI_ANTHROPIC,
        "http://127.0.0.1:9//",
        "",
    ] {
        assert_eq!(rule_base_url(input), old(input), "{input:?}");
    }
    assert_eq!(OFFICIAL_RULE_URL, ANTHROPIC_API_V1);
}

/// `--effort`（cli-args.md §1.4）：目录里的模型按它的档位取舍，显式覆盖原样写；没给时不改。
#[test]
fn the_effort_flag_sets_the_new_session_level() {
    let catalog = ModelCatalog {
        revision: 1,
        models: vec![entry("p1", GLM_5)],
        default: None,
    };
    let chosen = selection("p1", GLM_5, None);
    assert_eq!(
        with_effort(chosen.clone(), Some("high"), Some(&catalog)).reasoning,
        Some("high".to_owned())
    );
    assert_eq!(
        with_effort(chosen.clone(), Some("max"), Some(&catalog)).reasoning,
        Some("high".to_owned())
    );
    assert_eq!(
        with_effort(chosen.clone(), Some("medium"), Some(&catalog)).reasoning,
        None
    );
    assert_eq!(with_effort(chosen.clone(), None, Some(&catalog)), chosen);
    // 不在目录里的选择（不该出现）：没有可用的档位，不改。
    let unknown = selection("p9", "x", Some("low"));
    assert_eq!(
        with_effort(unknown.clone(), Some("high"), Some(&catalog)),
        unknown
    );
    assert_eq!(
        with_effort(selection("anthropic", GLM_5_3, None), Some("max"), None).reasoning,
        Some("max".to_owned())
    );
}

/// 只有内置配置、没有个人配置的平台层，注入了外部账号层（App 的 v5 模式，platform.md §12）。
async fn hosted_platform(
    home: &std::path::Path,
) -> (
    Arc<Platform>,
    rust_cli_kernel_api::external_accounts::ExternalAccounts,
) {
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), home, home);
    let origin = "http://127.0.0.1:9".to_owned();
    let services = rust_cli_platform::AccountServices {
        egress: Arc::new(
            rust_cli_net::Egress::new(rust_cli_net::NetworkPolicy::default())
                .await
                .unwrap(),
        ),
        clock: Arc::new(rust_cli_kernel_api::testing::ManualClock::new(0)),
        origins: rust_cli_platform::AccountOrigins {
            zcode_api: format!("{origin}/api/v1"),
            zai_api: origin.clone(),
            bigmodel_team_plans: format!("{origin}/coding-plan/team/plans"),
            bigmodel_api: origin,
            client_version: "0.0.0-test".to_owned(),
            usage_quota_url: None,
        },
        keys: std::sync::Arc::new(rust_cli_platform::testing::MemoryKeyStore::default()),
    };
    let secret = rust_cli_platform::CredentialSecret {
        explicit: secret("test-secret"),
        explicit_file: None,
        platform: rust_cli_platform::node_platform().to_owned(),
        home: home.to_string_lossy().into_owned(),
        username: "dev".to_owned(),
    };
    let accounts = rust_cli_kernel_api::external_accounts::ExternalAccounts::new();
    let platform =
        Platform::new(&paths, &secret, services).with_external_accounts(accounts.clone());
    (Arc::new(platform), accounts)
}

/// 有外部账号层时的组装（App 的 v5 模式）：缺省模型跟着格子（platform.md §12.2）。
async fn following(
    home: &std::path::Path,
) -> (
    Assembled,
    Arc<Platform>,
    rust_cli_kernel_api::external_accounts::ExternalAccounts,
) {
    let (platform, accounts) = hosted_platform(home).await;
    let assembled = assemble(
        platform.clone(),
        &Override {
            base_url: None,
            api_key: None,
            model: None,
        },
        &Assumptions::default(),
        (None, None),
        Some(accounts.clone()),
    )
    .await;
    (assembled, platform, accounts)
}

/// 平台层读的内置配置（这些测试不给显式文件：随包的一份），Host 推的账号层带它的摘要才对得上（22 §15.1）。
const BUNDLED_BUILTIN: &str =
    rust_cli_base::include_lf!("../../../platform/assets/zcode-builtin.json");

/// Host 推的一份账号层：摘要是平台层读的那份内置配置的。
fn layer(
    grants: rust_cli_kernel_api::external_accounts::AccountGrants,
) -> rust_cli_kernel_api::external_accounts::AccountLayer {
    rust_cli_kernel_api::external_accounts::AccountLayer {
        builtin_digest: rust_cli_base::hash::sha256_hex(BUNDLED_BUILTIN.as_bytes()),
        grants,
    }
}

fn individual_plan() -> rust_cli_kernel_api::external_accounts::AccountGrants {
    rust_cli_kernel_api::external_accounts::AccountGrants::from([(
        "account:zai-individual-coding-plan".to_owned(),
        rust_cli_kernel_api::external_accounts::AccountGrant {
            entitled: true,
            current: Some(true),
            models: None,
        },
    )])
}

/// 跟随的缺省模型（`wait`，App 当场等第一份）：返回定下的缺省与它的任务作用域（丢了作用域任务就停）。
async fn followed_default(
    assembled: Assembled,
    home: &std::path::Path,
) -> (DefaultModel, rust_cli_base::TaskScope) {
    let memory =
        rust_cli_paths::StoragePaths::resolve(&Default::default(), home, home).default_model();
    default_model(assembled.selection, memory, true, Instant::now()).await
}

/// 等缺省模型变成 `provider`（读的一方每次读现值）：跟随的任务重新选要读目录（文件 IO），测试按短间隔看，最多 5 s。
async fn until_default_is(default: &DefaultModel, provider: &str) {
    let seen = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while default.current().provider != provider {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(seen.is_ok(), "the default stayed {:?}", default.current());
}

/// 缺陷修复（22 §2.3 第 1 条）：App 的 v5 模式等 Host 的第一份账号层再选缺省模型；选不出时不写晚到原因（以前写死在模型端口里，之后每个请求都报它）。
/// 之后买了套餐：账号模型进目录，端点照样取得到。
#[tokio::test]
async fn the_default_waits_for_the_host_account_layer_and_never_latches_a_reason() {
    let home = tempfile::tempdir().unwrap();
    let (assembled, platform, accounts) = following(home.path()).await;
    assert!(matches!(assembled.selection, Chosen::Following(_)));
    let late = assembled.late.clone();
    let waiting = tokio::spawn({
        let home = home.path().to_path_buf();
        async move { followed_default(assembled, &home).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "waits for the first account layer");
    // 第一份：没有可用的账号套餐 → 选不出模型
    accounts.set(layer(Default::default()));
    let (default, _scope) = waiting.await.unwrap();
    assert!(default.current().provider.is_empty());
    assert!(late.get().is_none(), "no latched reason");
    accounts.set(layer(individual_plan()));
    let catalog = platform.catalog().await;
    assert!(
        catalog
            .models
            .iter()
            .any(|model| model.provider == "account:zai-individual-coding-plan"),
        "{catalog:?}"
    );
}

/// 同上（22 §3.5 第 6 项）：缺省模型按 Host 的第一份账号层选。选之前测试先把目录读几遍：不等账号层的写法这时已经选完（空），等的写法还在等。
#[tokio::test]
async fn the_default_is_chosen_from_the_first_account_layer() {
    let home = tempfile::tempdir().unwrap();
    let (assembled, platform, accounts) = following(home.path()).await;
    let waiting = tokio::spawn({
        let home = home.path().to_path_buf();
        async move { followed_default(assembled, &home).await }
    });
    for _ in 0..3 {
        let _ = platform.catalog().await;
        tokio::task::yield_now().await;
    }
    accounts.set(layer(individual_plan()));
    let (default, _scope) = waiting.await.unwrap();
    assert_eq!(
        default.current().provider,
        "account:zai-individual-coding-plan"
    );
}

/// 修 bug（ACCOUNT-OVERLAY 报的，platform.md §12.2）：启动后才登录的用户，进程级的缺省模型（任务工具的开关、旁路请求的兜底）跟着账号层换。
/// 以前只按第一份定一次，一直是空的，直到 App 重启。
#[tokio::test]
async fn the_default_follows_a_later_account_layer() {
    let home = tempfile::tempdir().unwrap();
    let (assembled, _platform, accounts) = following(home.path()).await;
    accounts.set(layer(Default::default()));
    let (default, _scope) = followed_default(assembled, home.path()).await;
    assert!(default.current().provider.is_empty(), "no plan at start");
    accounts.set(layer(individual_plan()));
    until_default_is(&default, "account:zai-individual-coding-plan").await;
    // 退出登录：又是空的
    accounts.set(layer(Default::default()));
    until_default_is(&default, "").await;
}
