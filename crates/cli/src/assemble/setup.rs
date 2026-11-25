//! 组装的步骤（[`crate::assemble`] 与账号、信任命令用）：平台层、出口、扩展、要读文件的参数与模型端口的入口级选项。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use rust_cli_base::clock::SystemClock;
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_env_local::{LocalEnvironment, LocalMcpTransport};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_net::Egress;
use rust_cli_node_store::NodeIds;
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_platform::{AccountServices, CredentialSecret, Platform, node_platform};
use rust_cli_runtime::{ExtConfig, Extensions};

use crate::assemble::env_layout::{EnvLayout, PrimaryPaths};
use crate::process::facts::{Facts, ProcessFacts};
use crate::process::output::{eprint_raw, print_raw};
use crate::{
    VERSION, assemble::mcp, assemble::network, assemble::options, assemble::origins,
    commands::args, commands::flags, commands::hooks_trust, commands::login, commands::update,
    config,
};
use rust_cli_kernel_api::controller_services::ControllerServices;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, PrimaryRegistry};
use rust_cli_kernel_api::machine::{Files, MachineEnv};

/// 进程环境（`zcode update` 与启动快照共用，F48）。
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程环境的位置（spec §4）"
)]
pub(crate) fn process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(key, value)| {
            let pair = (key.to_string_lossy(), value.to_string_lossy());
            (pair.0.into_owned(), pair.1.into_owned())
        })
        .collect()
}

/// Windows 上 `USERPROFILE` 没有或为空时的 home（Node `os.homedir()` 的系统 API）：`std::env::home_dir()` 在 Rust
/// 1.85 起不看 `HOME`，先 `USERPROFILE`、再 `GetUserProfileDirectoryW`（libuv `uv_os_get_passwd` 用的同一个 API）。
#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程环境的位置（spec §4）；Windows 的系统 home 只能经它取"
)]
pub(crate) fn windows_profile() -> Option<PathBuf> {
    std::env::home_dir()
}

/// 其他平台不用（paths 自己查系统用户数据库）。
#[cfg(not(windows))]
pub(crate) fn windows_profile() -> Option<PathBuf> {
    None
}

/// 平台层：Provider registry、账号（每请求鉴权、登录）与设置。
pub(crate) fn platform(
    facts: &Facts,
    paths: &StoragePaths,
    (egress, clock): (Arc<Egress>, Arc<SystemClock>),
    keychain_notice: Option<Arc<super::keys::KeychainNotice>>,
) -> Result<Platform> {
    let secret = CredentialSecret {
        explicit: facts.controller.credential_secret.clone(),
        explicit_file: facts.controller.credential_secret_file.clone(),
        platform: node_platform().to_owned(),
        // Node `os.homedir()`：与本机存储同一个 home（F57，config.md §2.3）。以前另取一份去掉首尾空白的变量。
        home: facts.process.home.to_string_lossy().into_owned(),
        username: facts.controller.user_name.clone(),
    };
    let env = |name: &str| facts.process.env.text(name).map(str::to_owned);
    let origins = origins::account_origins(env, VERSION).map_err(anyhow::Error::msg)?;
    let services = AccountServices {
        egress,
        clock,
        origins,
        keys: super::keys::system_keys(facts.hooks.keyring_file.as_ref(), keychain_notice),
    };
    // `ZCODE_MAX_OUTPUT_TOKENS`：Provider 配置的 `maxOutputTokens.map` 用 min(配置的 max, 它)（cli-args.md §9.1，F42）。
    Ok(Platform::new(paths, &secret, services)
        .with_max_output_tokens(facts.controller.max_output_tokens))
}

/// `ZCODE_AGENT_CA_CERT` 读不出时怎么办（config.md §2.4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnCaUnreadable {
    /// 启动就报错退出（交互界面、`-p`、执行器、`zcode login`：变量是用户在终端里给的）。
    Exit,
    /// 照常起来，每个出网请求报错（App：照 v4，进程退出会让整个 agent 不可用）。
    FailRequests,
}

/// 网络出口（config.md §2.4，F21）：按环境快照算出口策略；`NODE_EXTRA_CA_CERTS` 用不了时先在 stderr 写警告。
/// 进程里所有出网（模型、WebFetch、MCP HTTP、账号）共用这一个。
pub(crate) async fn egress(process: &ProcessFacts, on_ca: OnCaUnreadable) -> Result<Arc<Egress>> {
    const CONTEXT: &str = "configuring network egress";
    let network = network::network(&process.env, &process.cwd)
        .await
        .context(CONTEXT)?;
    for warning in &network.warnings {
        eprint_raw(warning);
    }
    let egress = match (network.policy, on_ca) {
        (Ok(policy), _) => Egress::new(policy).await.context(CONTEXT)?,
        (Err(ca), OnCaUnreadable::Exit) => return Err(anyhow!(ca.startup_error()).context(CONTEXT)),
        // 修 bug（SMALL-6，app-prefs.md §9 第 4 项）：以前 App 里设置页的 CA 读不出时 rust 启动就退出，监督器三次之后进 degraded，
        // 整个 agent 不可用、看不到原因。照 v4（Node 照常起来、每个请求报读文件的错误）：出口建不起客户端，每个请求报这句话。
        (Err(ca), OnCaUnreadable::FailRequests) => {
            tracing::warn!(
                event = "network.ca",
                reason = ca.reason(),
                "the CA file cannot be read; every outgoing request fails until the app restarts"
            );
            Egress::unavailable(ca.request_error())
        }
    };
    Ok(Arc::new(egress))
}

/// 自动更新的启动检查（docs/specs/rust-cli/update.md §3，F48）：只在交互界面调用。
pub(crate) async fn updates(
    env: &EnvSnapshot,
    config: &config::ConfigSnapshot,
    paths: &StoragePaths,
    (egress, clock): (&Arc<Egress>, &Arc<SystemClock>),
) -> Option<update::Updates> {
    let inputs = update::Inputs {
        env,
        enabled: config.check_for_update,
        version: VERSION,
        cache: paths.update_cache().as_path().to_path_buf(),
        egress: egress.clone(),
        clock: clock.clone(),
        registry: update::registry(),
    };
    Some(update::Updates::start(inputs).await)
}

/// `zcode update` / `zcode upgrade`（docs/specs/rust-cli/update.md §9）：先查最新版本，所以要出网。注册表的根是产品常量；
/// debug 构建里测试可以用 `_ZCODE_ASSUME_NPM_REGISTRY` 换成本地假注册表（同执行器的 npm 来源，release 构建不读）。
pub(crate) async fn update(command: update::Command) -> Result<u8> {
    let facts = Facts::capture()?;
    let egress = egress(&facts.process, OnCaUnreadable::Exit).await?;
    let registry = facts
        .hooks
        .machine_ssh
        .npm_registry
        .clone()
        .unwrap_or_else(update::registry);
    let lookup = update::Lookup {
        egress: &egress,
        registry: &registry,
    };
    update::command(
        command,
        &facts.process.env,
        VERSION,
        lookup,
        &mut std::io::stdout(),
    )
    .await
}

/// `zcode login` / `zcode logout`。
pub(crate) async fn account(command: login::Command) -> Result<u8> {
    let facts = Facts::capture()?;
    let paths = facts.storage_paths(&StorageSettings::default());
    let egress = egress(&facts.process, OnCaUnreadable::Exit).await?;
    let clock = Arc::new(SystemClock::new(facts.process.zone.clone()));
    // `zcode login` 不是交互界面：不提示（platform.md §13.12）
    let platform = platform(&facts, &paths, (egress, clock), None)?;
    Ok(login::run(command, &platform).await)
}

/// 主 env 就在本进程里的注册表（remote-exec.md §15.8.1；deployment.md §19.1）：`LocalEnvironment` 实现一台机器的 9 个
/// 端口与 controller 的取网页，MCP 传输是同一台机器的 `McpTransport` 端口。
pub(crate) fn environments(
    env: Arc<LocalEnvironment>,
    mcp: Arc<LocalMcpTransport>,
) -> Arc<dyn EnvironmentRegistry> {
    Arc::new(PrimaryRegistry::new(
        MachineEnv::new(env.clone(), mcp),
        ControllerServices::new(env),
    ))
}

/// 模型端口的入口级选项（TUI / `-p` 与 app-server 共用）：重试次数与非流式回退的开关（provider-errors.md §3.6、§4）、
/// 流空闲看门狗的上限（stall-visibility.md §2.2）、`IS_SANDBOX`
/// 下连续过载照常重试（cli-args.md §9）、`ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1）、模型请求的来源头与每请求头的
/// ID（照 Node，model.md §2.4）、设备身份（`metadata.user_id`，工作线 F52）与只给测试的 `_ZCODE_ASSUME_BASE_URL`
/// （provider-errors.md D13）。F129（审查 E10）：以前是对模型端口逐个调 `with_*`。
pub(crate) fn route_options(
    (env, max_output_tokens): (&EnvSnapshot, Option<u32>),
    ids: Arc<NodeIds>,
    (device, assumed_base_url): (rust_cli_model::DeviceIdSource, Option<String>),
) -> Result<rust_cli_model::RouteOptions> {
    let var = |name: &str| env.get(name).map(str::to_owned);
    let headers = crate::assemble::request_headers::request_headers(&var, VERSION, ids);
    let retry = rust_cli_model::RetryPolicy::default();
    Ok(rust_cli_model::RouteOptions {
        retry: rust_cli_model::RetryPolicy {
            max_retries: options::model_retries(env).unwrap_or(retry.max_retries),
            ..retry
        },
        disable_nonstreaming_fallback: options::nonstreaming_fallback_disabled(env),
        idle: options::stream_idle(env),
        sandbox: options::is_sandbox(env),
        max_output_tokens,
        headers: Some(Arc::new(headers.map_err(anyhow::Error::msg)?)),
        device: Some(device),
        assume_base_url: assumed_base_url,
        ..rust_cli_model::RouteOptions::default()
    })
}

/// `zcode hooks trust …`：工作区文件经本机执行环境读，信任记录经 platform（config.md §4.3）。
pub(crate) async fn trust(command: hooks_trust::TrustCommand) -> Result<u8> {
    let facts = Facts::capture()?;
    let paths = facts.storage_paths(&StorageSettings::default());
    let env = LocalEnvironment::new(Some(facts.process.home.clone()));
    // 记录的时间取自系统时钟（node-store 的 `SystemClock`，F7：入口也不直接读系统时间）。
    let now_ms = SystemClock::new(facts.process.zone.clone()).now_ms();
    let cwd = facts.process.cwd.to_string_lossy().into_owned();
    let context = hooks_trust::TrustContext {
        files: &env,
        paths: &paths,
        cwd: &cwd,
        version: VERSION,
        now_ms,
    };
    let output = hooks_trust::run(command, &context).await;
    print_raw(&output.stdout);
    eprint_raw(&output.stderr);
    Ok(output.code)
}

/// `zcode plugin …`（docs/specs/rust-cli/plugin-store.md §13）：本机的插件存储；市场与 zip 的下载经同一个出口。Ctrl+C 取消
/// 还在取内容或等锁的操作（提交之后照常做完，§12）。
pub(crate) async fn plugin(command: crate::plugin_admin::command::PluginCommand) -> Result<u8> {
    use std::io::IsTerminal;
    use tokio::io::AsyncBufReadExt;
    let facts = Facts::capture()?;
    let paths = facts.storage_paths(&StorageSettings::default());
    let egress = egress(&facts.process, OnCaUnreadable::Exit).await?;
    let clock = Arc::new(SystemClock::new(facts.process.zone.clone()));
    let home = facts.process.home.clone();
    let env = Arc::new(LocalEnvironment::new(Some(home.clone())));
    // 终端不是 App：前端撑不起来的插件同 TUI、`-p` 的加载
    let admin = crate::plugin_admin::PluginAdmin::new(
        (env, home),
        paths,
        facts.process.env.clone(),
        egress,
        (clock, crate::frontends::app::AppFrontend::Absent.unhosted()),
    );
    let stop = crate::process::signals::Stop::listen();
    let cwd = facts.process.cwd.to_string_lossy().into_owned();
    let confirm: crate::plugin_admin::command::Confirm = Box::new(|prompt: String| {
        Box::pin(async move {
            print_raw(&prompt);
            let mut line = String::new();
            let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
            reader.read_line(&mut line).await.is_ok()
                && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        })
    });
    let mut context = crate::plugin_admin::command::Context {
        admin: &admin,
        cwd: &cwd,
        interactive: std::io::stdin().is_terminal(),
        confirm,
        cancel: stop.token.clone(),
    };
    let output = crate::plugin_admin::command::run(command, &mut context).await;
    print_raw(&output.stdout);
    eprint_raw(&output.stderr);
    Ok(stop.code().unwrap_or(output.code))
}

/// skill、MCP 与插件的配置（docs/specs/rust-cli/ext.md §1）：MCP 服务器来自配置文件（`config`）与
/// `--mcp-config`，按 MCP 的机器绑定（旁路文件，remote-exec.md §22.2）设上所在的机器；`--plugin-dir` 相对会话的工作区。
/// 插件在这里加载成这套内核的 `PluginSet`（两段式组装的第二段，ext.md §4.0）：候选、配置值与数据根按 `layout`（执行侧）
/// 从配置快照算，前端撑不起来的插件按 App 前端层 `app` 跳过；前端层的端口也按这套内核的插件集在这里装（context.md §15.5）。
/// manifest 与组件经主 env 读。路径一律指主 env（deployment.md §21）：`--mcp-config` 与 `--plugin-dir` 按
/// `primary` 读、解析；`controller` 只读 controller 自己的配置文件（旁路文件）。`env` 是进程的环境快照。
pub(crate) async fn extensions(
    (primary, workspace, paths): (&MachineEnv, &WorkspaceRef, &PrimaryPaths),
    controller: &dyn Files,
    env: &EnvSnapshot,
    (flags, allowed, prewait): (&mcp::McpFlags, &[String], mcp::Prewait),
    (plugin_dirs, app, bindings): (
        &[String],
        &crate::frontends::app::AppFrontend,
        &rust_cli_paths::StoragePath,
    ),
    (config, layout, (mcp_sharing, mcp_client)): (
        &config::ConfigSnapshot,
        &EnvLayout,
        (rust_cli_ext::McpSharing, rust_cli_ext::McpClientTraits),
    ),
) -> Extensions {
    let mut extensions = Extensions::none();
    // MCP 服务器绑定到哪台机器（R5，remote-exec.md §22.2）：rust-cli 自己的旁路文件，是 controller 的配置。
    let path = bindings.as_path().to_string_lossy();
    let bindings =
        &crate::assemble::mcp_machines::McpBindings::load(controller, workspace, &path).await;
    let settings = mcp::settings(
        primary.files().as_ref(),
        workspace,
        (flags, allowed, prewait),
        (env, bindings),
        config.mcp_servers.clone(),
        config.mcp_enabled,
    )
    .await;
    // 配置里停用的服务器（tui-manage.md §6.1）：MCP 关闭或 `--strict-mcp-config` 时没有；同名的
    // `--mcp-config` 服务器遮住它。
    let mut mcp_disabled: Vec<_> = if config.mcp_enabled && !flags.strict {
        config
            .mcp_disabled
            .iter()
            .filter(|server| !settings.servers.iter().any(|on| on.name == server.name))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    // `/mcp enable` 之后它也连在绑定的机器上（R5，remote-exec.md §22.2）。
    bindings.bind(&mut mcp_disabled);
    let side = layout.plugin_side(&config.plugins.root);
    let sources = config::plugins::sources(&config.plugins, workspace.root.as_str(), side);
    let session_dirs: Vec<_> = plugin_dirs
        .iter()
        .map(|dir| paths.resolve(workspace, dir))
        .collect();
    // 前端撑不起来的插件不加载（visualize 要 Gen UI，context.md §15.7）
    let plugins = rust_cli_ext::plugins::load(
        primary.files().as_ref(),
        workspace,
        &session_dirs,
        (&sources, &app.unhosted()),
    )
    .await;
    extensions.frontend_prompt = Some(app.prompt(&plugins));
    extensions.config = ExtConfig {
        mcp_disabled,
        mcp: settings,
        plugins: std::sync::Arc::new(plugins),
        workspace: Some(workspace.clone()),
        mcp_sharing,
        mcp_client,
    };
    // stream-json 输入（headless.md §6.1）：Bash 后台命令的缺省期限随之不同。
    extensions.streaming_input = prewait.streaming_input;
    extensions
}

/// 要读文件、看目录的参数（cli-args.md §4、§6、§8）。
pub(crate) struct FlagInputs<'a> {
    pub(crate) system: &'a args::PromptFlags,
    pub(crate) append: &'a args::PromptFlags,
    pub(crate) add_dirs: &'a [String],
    pub(crate) settings: Option<&'a str>,
}

/// 读完的结果：两段 system prompt 的文字、加入的目录、`--settings` 的配置与要写到 stderr 的警告。
pub(crate) struct ResolvedFlags {
    pub(crate) system_prompt: Option<String>,
    pub(crate) append_system_prompt: Option<String>,
    pub(crate) add_dirs: Vec<String>,
    pub(crate) settings: Option<serde_json::Map<String, serde_json::Value>>,
    pub(crate) warnings: Vec<String>,
}

/// 路径一律指主 env（deployment.md §21）：连上主 env 之后经它的 `Files` 读文件、看目录，相对路径对着会话的工作区，
/// `~` 用主 env 的 home。主 env 就在本进程里时结果与以前在本机读相同。顺序：`--settings`、system prompt 文件、
/// 追加文件、`--add-dir`；出错时同样退出。
pub(crate) async fn resolve_flags(
    (primary, workspace, paths): (&MachineEnv, &WorkspaceRef, &PrimaryPaths),
    inputs: FlagInputs<'_>,
) -> Result<ResolvedFlags> {
    let place = flags::Place {
        files: primary.files().as_ref(),
        workspace,
        cwd: workspace.root.as_str(),
        home: paths.home.as_deref(),
        style: paths.style,
    };
    let settings = match inputs.settings {
        Some(value) => Some(flags::settings(&place, value).await?),
        None => None,
    };
    let system_prompt =
        flags::prompt_text(&place, inputs.system, flags::PromptKind::System).await?;
    let append_system_prompt =
        flags::prompt_text(&place, inputs.append, flags::PromptKind::Append).await?;
    let added = flags::add_dirs(&place, inputs.add_dirs).await;
    Ok(ResolvedFlags {
        system_prompt,
        append_system_prompt,
        add_dirs: added.dirs,
        settings,
        warnings: added.warnings,
    })
}

/// `--session-id` 的会话 ID（cli-args.md §7）：Node 的写法 `sess_<uuid>`（Node `createSessionId(id)`）；库里已有时按
/// 报 `Session ID <uuid> is already in use.`。
pub(crate) async fn fresh_session_id(
    store: &dyn rust_cli_kernel_api::store::SessionStore,
    uuid: &str,
) -> Result<rust_cli_kernel_api::ids::SessionId> {
    let id = rust_cli_kernel_api::ids::SessionId::new(format!("sess_{uuid}"));
    if store.load(&id).await.is_ok() {
        return Err(flags::Fatal(format!("Error: Session ID {uuid} is already in use.")).into());
    }
    Ok(id)
}

/// 设备身份的来源（platform.md §10，工作线 F52）：`<dataBaseDir>/.zcode/v2/telemetry-state.json` 的 `deviceMid`，与 App、
/// Node CLI 共用；新值由同一个 `NodeIds` 生成（README §4 的 ID 入口）。第一次请求时才解析，之后进程内同一个值。
pub(crate) fn device_id(paths: &StoragePaths, ids: Arc<NodeIds>) -> rust_cli_model::DeviceIdSource {
    use futures_util::FutureExt as _;
    let device = Arc::new(rust_cli_platform::local::DeviceMid::new(
        paths,
        Arc::new(move || ids.uuid()),
    ));
    Arc::new(move || {
        let device = device.clone();
        async move { device.get().await }.boxed()
    })
}
