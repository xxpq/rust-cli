//! 配置文件的组装（docs/specs/rust-cli/config.md）。
//!
//! hooks、权限规则、MCP、插件都读 Node CLI 与 App 共用的配置文件：用户级 `~/.zcode/cli/config.json`（经
//! platform，本机存储）与项目级每层的 `zcode.json`、`.zcode/config.json`（经执行环境，工作区文件；只读扩展声明，读进来时就投影，
//! config.md §1.1）。项目级一个 MCP 条目都没写时，
//! MCP 另读工作区根的回落文件 `.agents/mcp.json`；用户级一个 MCP 条目都没写时另读 `~/.agents/mcp.json`（docs/specs/rust-cli/ext.md §3.1）。
//! 结果是一份带版本的快照，交给内核时整份替换（config.md §3）。项目级 hooks 先经信任（`trust`）；项目级 MCP 声明的 digest
//! 在同一次读里算出（`mcp_trust`，App 形态按它准入，docs/zcode-protocol-v5/18-mcp-trust.md §12）；插件存储的意图在同一次组装里
//! 读（`plugins`，config.md §2.1、ext.md §4.12），候选在第二段按执行侧算。

mod files;
pub(crate) mod mcp_trust;
pub(crate) mod merge;
mod official_plugins;
pub(crate) mod plugins;
mod storage;
pub(crate) mod trust;

use rust_cli_ext::mcp::McpServer;
use rust_cli_hooks::node_config::NodeHooks;
use rust_cli_hooks::{HookSource, HooksConfig};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::machine::Files as FilesPort;
use rust_cli_kernel_api::permission::syntax::parse_rule;
use rust_cli_kernel_api::permission::{
    PermissionMode, PermissionRules, ProjectRules, RuleBehavior, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::store::LocalSettings;
use rust_cli_paths::StoragePaths;
use rust_cli_platform::local::{ConfigFile, read_cli_config, read_user_agents_mcp};
use serde_json::{Map, Value};

use self::files::{AgentsMcp, ProjectFile};
use self::merge::{Fallbacks, Layer};
pub(crate) use self::official_plugins::id as official_plugin_id;
pub(crate) use self::storage::StorageLayers;
use self::trust::{Pending, WorkspaceHooks};

/// 组装需要的输入（入口捕获的事实与端口）。
pub(crate) struct ConfigInputs<'a> {
    /// 项目级配置文件经本机的 `Files` 读（cli 从注册表的本机取，remote-exec.md §3）。
    pub(crate) files: &'a dyn FilesPort,
    pub(crate) workspace: &'a WorkspaceRef,
    pub(crate) paths: &'a StoragePaths,
    /// 入口捕获的进程环境（http hook 的 header 插值用）。
    pub(crate) process_env: &'a rust_cli_base::secret::EnvSnapshot,
    /// `--settings` 给的配置（docs/specs/rust-cli/cli-args.md §8）：Node 配置的同一种写法，排在项目级之后（文件层里
    /// 优先级最高），命令行的单项参数仍在它之上。
    pub(crate) flag: Option<&'a Map<String, Value>>,
    /// 项目级配置文件在不在本机（主 env 就在本进程里）。不在时 `--settings` 也是执行侧的文件，不能改 controller 的存储位置：
    /// 存储键只认用户级配置与环境变量（deployment.md §21）。项目文件本来就不带存储键（config.md §1.1）。
    pub(crate) project_is_local: bool,
    /// App 的共享进程（一个进程服务所有本地工作区，工作线 F145，config.md §2.6）：只能有一个会话库，`--settings` 的
    /// `storage.*` 不读、记警告（项目文件本来就不带，config.md §1.1）。
    pub(crate) shared_process: bool,
}

/// `--settings` 一层在警告里的名字。
const FLAG_LAYER: &str = "--settings";

/// 配置的带版本快照（config.md §3）。
#[derive(Debug, Clone)]
pub(crate) struct ConfigSnapshot {
    pub(crate) version: u64,
    pub(crate) hooks: HooksConfig,
    /// 配置文件的 `allowedTools` / `disallowedTools`（来源是最后写它的层）。
    pub(crate) rules: Vec<SourcedRule>,
    /// 配置文件的 `permission.mode`。
    pub(crate) mode: Option<PermissionMode>,
    /// 自动模式的设置（`permission.useAutoModeDuringPlan`、`permission.autoMode`、`permission.disableAutoMode`；auto-classifier.md
    /// §16.6）：随规则快照交判决链。
    pub(crate) auto_mode: rust_cli_kernel_api::permission::auto_mode::AutoModeSettings,
    /// 用户级与项目级的 MCP 服务器（已合并、已去掉停用的）。
    pub(crate) mcp_servers: Vec<McpServer>,
    /// 配置里停用的 MCP 服务器与每个名字的来源文件（`/mcp enable/disable`，tui-manage.md §6.1）。
    pub(crate) mcp_disabled: Vec<McpServer>,
    pub(crate) mcp_origins: std::collections::BTreeMap<String, merge::McpOrigin>,
    /// `features.mcp` 不为 false。
    pub(crate) mcp_enabled: bool,
    /// 启用但未信任的项目 hooks（`-p` 结束时提示，config.md §4.4）。
    pub(crate) pending: Option<Pending>,
    /// 插件的段与意图（config.md §2.1、ext.md §4.0）：候选、配置值与数据根在第二段按执行侧算。
    pub(crate) plugins: plugins::PluginConfig,
    /// 界面语言 `ui.locale`（tui.md §7.1）。
    pub(crate) locale: Option<rust_cli_base::ui_prefs::UiLocale>,
    /// 启动时检查新版本 `checkForUpdateOnStartup`（update.md §10，F48；缺省 true）。
    pub(crate) check_for_update: bool,
    /// 提问框的空闲超时 `askUserQuestionTimeout`（毫秒，tools-more.md §1.1，工作线 F72；缺省不计时）。
    pub(crate) question_timeout: Option<u64>,
    /// `compact.autoThresholdTokens`（自动压缩的位置，compact.md §2.3）。
    pub(crate) compact_at: Option<u64>,
    /// 定了 `compact_at` 的文件，当它在用户级之后的层（`--settings`）时：写回用户级配置改不了它。
    pub(crate) compact_pinned: Option<String>,
    /// 配置文件来源的规则来自的文件：最后写那个键的一层（tui-manage.md §1.2；项目文件不带 `permission`，所以是用户级文件）。
    /// `--settings` 不算。
    pub(crate) rule_files: Vec<(RuleSource, RuleBehavior, String)>,
    /// 会话库与存储根（`storage.sessionDbPath`、`storage.dir`，修复线 F2）：入口按它解析库与产物目录的位置。
    pub(crate) storage: StorageLayers,
    /// 项目级 MCP 声明的 digest 与信任记录的位置（与 `mcp_servers` 同一次读，18 §12.2）：App 形态的准入端口；TUI 与 `-p` 不用。
    pub(crate) mcp_trust: mcp_trust::WorkspaceMcpTrust,
}

impl ConfigSnapshot {
    /// 判决链的规则快照：配置文件、本机项目规则集（LocalSettings）与命令行（shell-permissions.md §5），加配置文件的自动模式设置。
    pub(crate) fn permission_rules(
        &self,
        local: Vec<SourcedRule>,
        cli: PermissionRules,
    ) -> PermissionRules {
        PermissionRules {
            version: self.version,
            rules: [self.rules.clone(), local, cli.rules].concat(),
            auto_mode: self.auto_mode.clone(),
            ..cli
        }
    }
}

/// 本机项目规则集（LocalSettings 来源，shell-permissions.md §5；另有记住的附加工作目录，permission-updates.md §6.2）；读不出时
/// 为空并记警告。
pub(crate) async fn local_rules(
    store: &dyn LocalSettings,
    workspace: &WorkspaceRef,
) -> ProjectRules {
    match store.project_rules(workspace).await {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(%error, "the project permission ruleset could not be read");
            ProjectRules::default()
        }
    }
}

/// TUI 与 `-p` 的内核缺省的附加工作目录（permission-updates.md §6.2）：`--add-dir` 在前，接上本机项目规则集里记住的；相对路径、
/// 已在工作区根或前面的目录里的不收（`working_dirs::admit`，同建会话）。App 的共享进程不读记住的（附加目录的 owner 是 Host）。
pub(crate) fn default_directories(
    root: &EnvPath,
    add_dirs: &[String],
    remembered: &[EnvPath],
) -> Vec<EnvPath> {
    let entries = add_dirs
        .iter()
        .map(EnvPath::new)
        .chain(remembered.iter().cloned());
    rust_cli_kernel_api::working_dirs::admit(root, entries)
}

/// 新会话的模式（config.md §2.2，Node `options.runtimeConfig.mode ?? persistedMode ??
/// config.permission.mode`）：命令行 → 本机项目的持久模式 → 配置文件 → default。命令行给了时不读持久模式；
/// 持久模式读不出时当作没有并记警告。
pub(crate) async fn initial_mode(
    cli: Option<PermissionMode>,
    config: Option<PermissionMode>,
    store: &dyn LocalSettings,
    workspace: &WorkspaceRef,
) -> PermissionMode {
    if let Some(mode) = cli {
        return mode;
    }
    let project = match store.project_mode(workspace).await {
        Ok(mode) => mode,
        Err(error) => {
            tracing::warn!(%error, "the project permission mode could not be read");
            None
        }
    };
    project.or(config).unwrap_or(PermissionMode::Default)
}

fn sourced(source: RuleSource, behavior: RuleBehavior, texts: &[String]) -> Vec<SourcedRule> {
    texts
        .iter()
        .map(|text| SourcedRule {
            source,
            behavior,
            rule: parse_rule(text),
        })
        .collect()
}

/// 读不出的文件记警告（Node `config_file_invalid`）。
fn object<'a>(
    path: &str,
    file: &'a ConfigFile,
    warnings: &mut Vec<String>,
) -> Option<&'a Map<String, Value>> {
    if let ConfigFile::Invalid { reason } = file {
        warnings.push(format!("{path}: the config file is ignored: {reason}"));
    }
    file.object()
}

/// 用户级与项目级的配置文件（启动组装与 `zcode hooks trust` 共用）。
pub(crate) struct Files {
    pub(crate) user_path: String,
    pub(crate) user: ConfigFile,
    pub(crate) projects: Vec<ProjectFile>,
    /// 工作区根的 `.agents/mcp.json`（存在时）：项目级 MCP 声明的回落，不是一层配置。
    pub(crate) agents: Option<AgentsMcp>,
    /// home 下的 `~/.agents/mcp.json`（存在时）：用户级 MCP 声明的回落（v4 App 的规则），不是一层配置。
    pub(crate) user_agents: Option<AgentsMcp>,
}

impl Files {
    /// 回落给 MCP 的合并的工作区根 `.agents/mcp.json`：有项目配置文件读不出时不给（Node 的 `discover` 读项目配置时就抛错，读不到回落；
    /// 读不出的文件里可能写了服务器）。项目配置有没有条目由合并判断（`merge::mcp_layers`）。
    pub(crate) fn fallback(&self) -> Option<&AgentsMcp> {
        let readable = !self
            .projects
            .iter()
            .any(|project| matches!(project.file, ConfigFile::Invalid { .. }));
        self.agents.as_ref().filter(|_| readable)
    }

    /// 两个回落：工作区根的（[`Self::fallback`]）与 home 下的。用户级配置读不出时不给后者（同上：读不出的文件里可能写了服务器）。
    pub(crate) fn fallbacks(&self) -> Fallbacks<'_> {
        let readable = !matches!(self.user, ConfigFile::Invalid { .. });
        Fallbacks {
            project: self.fallback(),
            user: self.user_agents.as_ref().filter(|_| readable),
        }
    }
}

/// 读 home 下的 `~/.agents/mcp.json`（经 platform，同用户级配置）；不存在时为 None。用户级的服务器不锚定相对的 `cwd`。
async fn user_agents_mcp(paths: &StoragePaths) -> Option<AgentsMcp> {
    let file = read_user_agents_mcp(paths).await;
    (file != ConfigFile::Missing).then(|| AgentsMcp {
        path: paths.user_agents_mcp().as_path().display().to_string(),
        anchor: None,
        file,
    })
}

/// 用户级与项目级的配置层（读不出的文件不成层、记警告）：`empty` 顶替没有或读不出的用户级配置。
fn file_layers<'a>(
    files: &'a Files,
    empty: &'a Map<String, Value>,
    warnings: &mut Vec<String>,
) -> Vec<Layer<'a>> {
    let user = object(&files.user_path, &files.user, warnings);
    let mut layers = vec![Layer {
        source: RuleSource::UserSettings,
        object: user.unwrap_or(empty),
        path: &files.user_path,
    }];
    for project in &files.projects {
        if let Some(object) = object(&project.path, &project.file, warnings) {
            layers.push(Layer {
                source: RuleSource::ProjectSettings,
                object,
                path: &project.path,
            });
        }
    }
    layers
}

pub(crate) async fn read_files(inputs: &ConfigInputs<'_>) -> Files {
    Files {
        user_path: inputs.paths.cli_config().as_path().display().to_string(),
        user: read_cli_config(inputs.paths).await,
        projects: files::discover(inputs.files, inputs.workspace).await,
        agents: files::agents_mcp(inputs.files, inputs.workspace).await,
        user_agents: user_agents_mcp(inputs.paths).await,
    }
}

/// 读配置文件并组装快照。只在启动时调用一次（没有文件监听，config.md §6 D6）。
pub(crate) async fn assemble(inputs: &ConfigInputs<'_>) -> ConfigSnapshot {
    let mut warnings = Vec::new();
    let files = read_files(inputs).await;
    let empty = Map::new();
    let mut layers = file_layers(&files, &empty, &mut warnings);
    let Files {
        user_path,
        user,
        projects,
        ..
    } = &files;
    let user_object = user.object();
    // `--settings`（flagSettings，排在 user / project / local 之后）：规则的来源记为命令行（cli-args.md §8、D7）。
    if let Some(object) = inputs.flag {
        layers.push(Layer {
            source: RuleSource::CliArg,
            object,
            path: FLAG_LAYER,
        });
    }
    let permission = merge::permission(&layers, &mut warnings);
    let mut rules = Vec::new();
    if let Some((source, texts)) = &permission.allowed {
        rules.extend(sourced(*source, RuleBehavior::Allow, texts));
    }
    if let Some((source, texts)) = &permission.disallowed {
        rules.extend(sourced(*source, RuleBehavior::Deny, texts));
    }
    let mcp_layers = merge::mcp_layers(&layers, files.fallbacks(), &mut warnings);
    let mcp_trust = mcp_trust::WorkspaceMcpTrust::of(
        inputs.workspace.root.as_str(),
        &files,
        &mcp_layers,
        inputs.paths,
    );
    let merge::McpLayers {
        servers: mcp_servers,
        disabled: mcp_disabled,
        origins: mcp_origins,
    } = mcp_layers;
    let mcp_enabled = merge::mcp_enabled(&layers);
    let locale = merge::locale(&layers, &mut warnings);
    let check_for_update = merge::check_for_update(&layers, &mut warnings);
    let question_timeout = merge::question_timeout(&layers, &mut warnings);
    let (compact_at, compact_pinned) = merge::compact_at(&layers, &mut warnings);
    let root = inputs.workspace.root.as_str();
    let plugin_section = plugins::merge(&layers, &mut warnings);
    let storage = if inputs.shared_process {
        let own = storage::controller_layers(&layers, storage::SHARED_PROCESS, &mut warnings);
        StorageLayers::from_layers(&own, inputs.process_env)
    } else if inputs.project_is_local {
        StorageLayers::from_layers(&layers, inputs.process_env)
    } else {
        let own = storage::controller_layers(&layers, storage::REMOTE_PRIMARY, &mut warnings);
        StorageLayers::from_layers(&own, inputs.process_env)
    };
    let base = std::path::Path::new(root);
    let plugin_root = inputs.paths.plugin_storage(storage.dir.as_deref(), base);
    // 插件存储的读侧（ext.md §4.12）：本机实现；云上的实现接进来时在这里按部署选。
    let store = plugins::LocalPluginStore {
        root: plugin_root.clone(),
        secrets: inputs.paths.plugin_secrets(storage.dir.as_deref(), base),
        builtin: inputs
            .process_env
            .text(rust_cli_base::names::env::ZCODE_BUILTIN_PLUGINS_DIR)
            .map(std::path::PathBuf::from),
    };
    let intents = plugins::PluginStore::intents(&store).await;
    let plugins = plugins::PluginConfig {
        section: plugin_section,
        intents,
        root: plugin_root,
    };
    let user_hooks = match user_object.and_then(|object| object.get("hooks")) {
        Some(value) => {
            let (parsed, found) = NodeHooks::from_json(value);
            warnings.extend(found.into_iter().map(|w| format!("{user_path}: {w}")));
            parsed
        }
        None => NodeHooks::default(),
    };
    let (user_settings, found) = user_hooks.user_settings();
    warnings.extend(found);
    // 项目级 hooks：只有已信任且启用的声明进快照（config.md §4.2）。
    let workspace = WorkspaceHooks::inspect(
        inputs.workspace.root.as_str(),
        user_object,
        projects,
        inputs.paths,
        &mut warnings,
    )
    .await;
    let mut sources = vec![(HookSource::User, user_settings)];
    sources.extend(workspace.admitted());
    // `--settings` 的 hooks：命令行上给的，不经项目 hooks 的信任（同用户级）。
    if let Some(value) = inputs.flag.and_then(|object| object.get("hooks")) {
        let (parsed, found) = NodeHooks::from_json(value);
        warnings.extend(found.into_iter().map(|w| format!("{FLAG_LAYER}: {w}")));
        let (settings, found) = parsed.user_settings();
        warnings.extend(found);
        sources.push((HookSource::User, settings));
    }
    let hooks = HooksConfig {
        version: 1,
        sources,
        trusted: true,
        env: inputs.process_env.clone(),
        prompt_model: None,
    };
    for warning in &warnings {
        tracing::warn!(warning, "configuration");
    }
    ConfigSnapshot {
        version: 1,
        hooks,
        rules,
        mode: permission.mode,
        auto_mode: permission.auto_mode.clone(),
        mcp_servers,
        mcp_disabled,
        mcp_origins,
        mcp_enabled,
        pending: workspace.pending(),
        plugins,
        locale,
        check_for_update,
        question_timeout,
        compact_at,
        compact_pinned,
        rule_files: rule_files(&permission),
        storage,
        mcp_trust,
    }
}

fn rule_files(permission: &merge::Permission) -> Vec<(RuleSource, RuleBehavior, String)> {
    [
        (
            RuleBehavior::Allow,
            &permission.allowed,
            &permission.allowed_file,
        ),
        (
            RuleBehavior::Deny,
            &permission.disallowed,
            &permission.disallowed_file,
        ),
    ]
    .into_iter()
    .filter_map(|(behavior, rules, file)| {
        // 项目文件不带 `permission`（config.md §1.1），配置文件来源只剩用户级；`--settings` 的不算。
        let (source, _) = rules.as_ref()?;
        (*source == RuleSource::UserSettings)
            .then(|| file.clone().map(|path| (*source, behavior, path)))
            .flatten()
    })
    .collect()
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
