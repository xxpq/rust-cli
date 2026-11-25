//! 外部账号层（platform.md §12）：有它时账号 provider 的权益、当前连接与模型白名单以它为准，不读凭据、不发账号请求；
//! 团队不平铺；闲时、高速卡没有这一轮的执行材料时报稳定的鉴权失败，不向来源取凭据。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::hash::sha256_hex;
use rust_cli_base::models;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::external_accounts::{
    AccountGrant, AccountGrants, AccountLayer, ExternalAccounts,
};
use rust_cli_kernel_api::model::{ModelConnection, ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{
    EndpointQuery, HeaderRequest, HeaderSource, ModelDirectory, ModelEndpoints, SuppliedAuth,
};
use rust_cli_kernel_api::testing::ManualClock;
use rust_cli_paths::StoragePaths;
use serde_json::json;

use crate::Platform;
use crate::fake_server_tests::{Harness, NOW_MS, Reply, Server, ZAI, secret};
use crate::providers::BUNDLED_BUILTIN;
use crate::test_support::write;

const START: &str = "account:zai-start-plan";
const TEAM: &str = "account:zai-team-coding-plan";
const HIGHSPEED: &str = "account:zai-highspeed-card";
/// 显式内置配置文件的名字（临时目录里）。
const BUILTIN_FILE: &str = "builtin-provider-config.json";
/// 另一份内置配置的摘要：Host 依据的不是平台层读到的这份。
const OTHER_BUILTIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const PERSONAL: &str = include_str!("../tests/fixtures/personal.json");
/// Start Plan 的内置模型里不在白名单里的一个。
const NOT_LISTED: &str = models::GLM_5_3_FLASH_ACCOUNT;
const LISTED: &str = models::GLM_5_2_ACCOUNT;

/// 记下每次取凭据的来源。
#[derive(Default)]
struct Source(Mutex<Vec<HeaderRequest>>);

#[async_trait]
impl HeaderSource for Source {
    async fn request_auth(&self, request: HeaderRequest) -> Result<SuppliedAuth, ModelError> {
        self.0.lock().unwrap().push(request);
        Ok(SuppliedAuth {
            api_key: Some(RedactedString::new("sk-host")),
            ..SuppliedAuth::default()
        })
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    paths: StoragePaths,
    server: Server,
    accounts: ExternalAccounts,
    source: Arc<Source>,
    platform: Platform,
}

/// 平台层注入了外部账号层与取凭据的来源；账号服务的假服务对任何请求都回 500（只为数请求）。凭据里有 zai 的 OAuth 登录：
/// 没有外部账号层时这会让平台层去查个人订阅与团队。内置配置是随包的一份。
async fn setup() -> Setup {
    setup_with(false).await
}

/// 同 [`setup`]；`builtin_file` 时平台层读显式的内置配置文件（App 里 Host 拉起时给的 Active 文件），内容先是随包的一份。
async fn setup_with(builtin_file: bool) -> Setup {
    let server = Server::start(|_| Reply::json(500, &json!({}))).await;
    let dir = tempfile::tempdir().unwrap();
    let mut paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    if builtin_file {
        let file = dir.path().join(BUILTIN_FILE);
        write(&file, BUNDLED_BUILTIN).await;
        paths = paths.with_builtin_provider_config(&file);
    }
    let clock = Arc::new(ManualClock::new(NOW_MS));
    let accounts = ExternalAccounts::new();
    let source = Arc::new(Source::default());
    let platform = Harness::platform_with(&paths, &server.url(), clock, &secret())
        .await
        .with_header_source(source.clone())
        .with_external_accounts(accounts.clone());
    write(
        paths.credentials().as_path(),
        json!({
            "oauth:zai:access_token": "zai-access",
            "oauth:zai:user_info": json!({"user_id": "u1", "name": "dev"}).to_string(),
        })
        .to_string(),
    )
    .await;
    Setup {
        _dir: dir,
        paths,
        server,
        accounts,
        source,
        platform,
    }
}

impl Setup {
    /// Host 推一份账号层，摘要是随包的那份内置配置的（平台层读的就是它，或者是内容同它的显式文件）：对得上（22 §15.1）。
    fn push(&self, grants: AccountGrants) -> bool {
        self.push_for(&sha256_hex(BUNDLED_BUILTIN.as_bytes()), grants)
    }

    /// Host 推一份按摘要是 `builtin_digest` 的内置配置算的账号层。
    fn push_for(&self, builtin_digest: &str, grants: AccountGrants) -> bool {
        self.accounts.set(AccountLayer {
            builtin_digest: builtin_digest.to_owned(),
            grants,
        })
    }
}

/// Start Plan 的一项：白名单是 [`LISTED`]，是当前连接。
fn start_plan(entitled: bool) -> AccountGrants {
    AccountGrants::from([(
        START.to_owned(),
        AccountGrant {
            models: Some(vec![LISTED.to_owned()]),
            ..grant(entitled, Some(true))
        },
    )])
}

fn grant(entitled: bool, current: Option<bool>) -> AccountGrant {
    AccountGrant {
        entitled,
        current,
        models: None,
    }
}

async fn names(platform: &Platform) -> Vec<String> {
    platform
        .catalog()
        .await
        .models
        .iter()
        .map(|model| format!("{}/{}", model.provider, model.model))
        .collect()
}

#[tokio::test]
async fn the_external_layer_decides_the_account_providers() {
    let setup = setup().await;
    // 第一份到之前：账号 provider 一律不授权
    assert!(names(&setup.platform).await.is_empty());
    let first = setup.platform.catalog().await.revision;

    setup.push(AccountGrants::from([
        (
            START.to_owned(),
            AccountGrant {
                models: Some(vec![LISTED.to_owned()]),
                ..grant(true, Some(true))
            },
        ),
        // 授权了但不是当前连接：不可执行
        (ZAI.to_owned(), grant(true, Some(false))),
        (TEAM.to_owned(), grant(true, Some(true))),
    ]));
    let catalog = setup.platform.catalog().await;
    assert!(catalog.revision > first);
    let listed = names(&setup.platform).await;
    assert!(listed.contains(&format!("{START}/{LISTED}")), "{listed:?}");
    assert!(
        !listed.contains(&format!("{START}/{NOT_LISTED}")),
        "{listed:?}"
    );
    assert!(
        !listed.iter().any(|name| name.starts_with(ZAI)),
        "{listed:?}"
    );
    // 团队不平铺：模型一份，不带团队项目（团队由 Host 的当前连接定）
    let team: Vec<_> = catalog
        .models
        .iter()
        .filter(|model| model.provider == TEAM)
        .collect();
    assert!(!team.is_empty());
    assert!(team.iter().all(|model| model.connection.is_none()));

    // 同内容的一份不加版本
    let revision = catalog.revision;
    assert!(!setup.push(AccountGrants::from([
        (
            START.to_owned(),
            AccountGrant {
                models: Some(vec![LISTED.to_owned()]),
                ..grant(true, Some(true))
            },
        ),
        (ZAI.to_owned(), grant(true, Some(false))),
        (TEAM.to_owned(), grant(true, Some(true))),
    ])));
    assert_eq!(setup.platform.catalog().await.revision, revision);
    // 不查个人订阅、团队列表与权益
    assert!(
        setup.server.requests().is_empty(),
        "{:?}",
        setup.server.requests()
    );
}

#[tokio::test]
async fn the_external_layer_reads_no_credentials() {
    let setup = setup().await;
    // 凭据文件坏了：没有外部账号层时账号 provider 全部不授权（§5.1）；有了就不看它
    write(setup.paths.credentials().as_path(), "not json").await;
    setup.push(AccountGrants::from([(
        ZAI.to_owned(),
        grant(true, Some(true)),
    )]));
    let listed = names(&setup.platform).await;
    assert!(
        listed.contains(&format!("{ZAI}/{}", models::GLM_5_3_ACCOUNT)),
        "{listed:?}"
    );
}

#[tokio::test]
async fn a_team_selection_takes_the_team_model_the_host_chooses() {
    let setup = setup().await;
    setup.push(AccountGrants::from([(
        TEAM.to_owned(),
        grant(true, Some(true)),
    )]));
    // TUI 建的团队会话带着团队项目：外部账号层下团队不平铺，按 provider + 模型找
    let selection = ModelSelection {
        connection: Some(Box::new(ModelConnection {
            product_id: "product".to_owned(),
            organization_id: "org".to_owned(),
            project_id: "project".to_owned(),
        })),
        ..ModelSelection::new(TEAM, models::GLM_5_3_ACCOUNT)
    };
    setup
        .platform
        .endpoint(EndpointQuery::new(&selection))
        .await
        .unwrap();
    let asked = setup.source.0.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].mode, crate::account::TEAM_CODING_PLAN);
}

#[tokio::test]
async fn the_start_plan_asks_the_injected_source() {
    let setup = setup().await;
    setup.push(AccountGrants::from([(
        START.to_owned(),
        AccountGrant {
            models: Some(vec![LISTED.to_owned()]),
            ..grant(true, Some(true))
        },
    )]));
    setup
        .platform
        .endpoint(EndpointQuery::new(&ModelSelection::new(START, LISTED)))
        .await
        .unwrap();
    let asked = setup.source.0.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].mode, "start-plan");
}

#[tokio::test]
async fn per_turn_modes_fail_without_per_turn_credentials() {
    let setup = setup().await;
    // 闲时、高速卡不定义当前连接（缺省按「是」）
    setup.push(AccountGrants::from([(
        HIGHSPEED.to_owned(),
        grant(true, None),
    )]));
    let error = setup
        .platform
        .endpoint(EndpointQuery::new(&ModelSelection::new(
            HIGHSPEED,
            models::GLM_5_3_ACCOUNT,
        )))
        .await
        .unwrap_err();
    match error {
        ModelError::Authentication { message } => {
            assert!(
                message.starts_with(crate::account::MODEL_REQUEST_AUTH_MISSING),
                "{message}"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    // 不向来源取普通套餐的凭据
    assert!(setup.source.0.lock().unwrap().is_empty());
}

/// 这一轮带着凭据引用（MODEL-EXEC-LINK，§12.2）：闲时、高速卡向来源要凭据，引用原样带上（来源按它现取）。以前不看引用、一律拒绝。
#[tokio::test]
async fn per_turn_modes_ask_the_source_with_the_reference() {
    let setup = setup().await;
    setup.push(AccountGrants::from([(
        HIGHSPEED.to_owned(),
        grant(true, None),
    )]));
    let selection = ModelSelection::new(HIGHSPEED, models::GLM_5_3_ACCOUNT);
    setup
        .platform
        .endpoint(EndpointQuery {
            execution: Some("card-1"),
            ..EndpointQuery::new(&selection)
        })
        .await
        .unwrap();
    let asked = setup.source.0.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].mode, "highspeed");
    assert_eq!(asked[0].execution.as_deref(), Some("card-1"));
}

/// 内置配置摘要（22 §15.4）：账号层依据的内置配置与平台层读到的不同（Host 换了 Active 而这里还读着旧的，或反过来）——不用这份账号层，
/// 已发布的组合整份不变（版本不加），端点照旧按它取。照 Node 的 basedOn 门：不发布「这份内置 + 别的内置上算的账号层」。
#[tokio::test]
async fn a_layer_for_another_builtin_keeps_the_published_combination() {
    let setup = setup().await;
    setup.push(start_plan(true));
    let before = setup.platform.catalog().await;
    let listed = names(&setup.platform).await;
    assert!(listed.contains(&format!("{START}/{LISTED}")), "{listed:?}");
    // 另一份内置配置上算的、Start Plan 不授权的一份：格子照收，平台层不用
    assert!(setup.push_for(OTHER_BUILTIN, start_plan(false)));
    let after = setup.platform.catalog().await;
    assert_eq!(after.revision, before.revision);
    let listed = names(&setup.platform).await;
    assert!(listed.contains(&format!("{START}/{LISTED}")), "{listed:?}");
    setup
        .platform
        .endpoint(EndpointQuery::new(&ModelSelection::new(START, LISTED)))
        .await
        .unwrap();
}

/// 内置配置文件换了（Host 的 CDN 换代，22 §15.5 A）：还按旧文件算的一份不用（沿用旧组合）；按新文件算的一份对上，新组合生效。
#[tokio::test]
async fn a_new_builtin_file_takes_the_layer_with_its_digest() {
    let setup = setup_with(true).await;
    let file = setup
        .paths
        .builtin_provider_config()
        .unwrap()
        .as_path()
        .to_path_buf();
    let first = sha256_hex(BUNDLED_BUILTIN.as_bytes());
    setup.push_for(&first, start_plan(false));
    assert!(names(&setup.platform).await.is_empty());
    let revision = setup.platform.catalog().await.revision;
    // Host 换代：同内容、修订号 +1（字节不同）
    let mut release: serde_json::Value = serde_json::from_str(BUNDLED_BUILTIN).unwrap();
    release["revision"] = json!(release["revision"].as_u64().unwrap() + 1);
    let next = serde_json::to_string(&release).unwrap();
    write(&file, &next).await;
    // Host 还按旧文件算：授权了也不用
    setup.push_for(&first, start_plan(true));
    assert!(names(&setup.platform).await.is_empty());
    assert_eq!(setup.platform.catalog().await.revision, revision);
    // 按新文件算的一份：对上
    setup.push_for(&sha256_hex(next.as_bytes()), start_plan(true));
    let listed = names(&setup.platform).await;
    assert!(listed.contains(&format!("{START}/{LISTED}")), "{listed:?}");
}

/// 第一次读就对不上（22 §15.4 第 4 行）：发布不授权的账号层，个人配置的 provider 照常可用（照 Node 进程启动时的那一份）；之后对上的一份到了就用。
#[tokio::test]
async fn a_mismatch_before_anything_is_published_keeps_the_personal_providers() {
    let setup = setup().await;
    write(setup.paths.provider_config().as_path(), PERSONAL).await;
    setup.push_for(OTHER_BUILTIN, start_plan(true));
    let listed = names(&setup.platform).await;
    assert!(
        listed.contains(&format!("personal:gateway/{}", models::GLM_5)),
        "{listed:?}"
    );
    assert!(
        !listed.iter().any(|name| name.starts_with(START)),
        "{listed:?}"
    );
    setup.push(start_plan(true));
    let listed = names(&setup.platform).await;
    assert!(listed.contains(&format!("{START}/{LISTED}")), "{listed:?}");
}
