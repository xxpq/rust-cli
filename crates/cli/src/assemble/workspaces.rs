//! App 的共享进程：每个工作区一套内核（工作线 F145，docs/specs/rust-cli/shared-workspaces.md §3、§6）。
//!
//! 进程级的部件（端口：模型、会话库、执行环境、时钟与 ID；存储位置）在 [`super::open`] 与 [`super::ports`] 里建一次。
//! 这里按工作区组装其余的：用户层 + 该工作区的项目层配置 → 缺省模式、hooks、权限规则（配置 + 本机持久规则）、插件
//! （项目层的 `plugins.dirs` 不读）、MCP 声明（项目级的按信任记录准入，docs/zcode-protocol-v5/18-mcp-trust.md §12.4）、
//! 自动记忆目录、定时任务存储，得到一套 `LocalKernel`。`hooks.trustGrant` 写预信任与之后重读 hooks 也在这里（config.md §4.5）。步骤与 TUI 的 [`super::kernel`] 是同一批函数，少了只给终端的
//! 几步（root 下的 bypass、远端机器、跨会话通信、命令行参数）。会话标题照 TUI 生成（规格的 `titles`，titles.md §1）。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_base::clock::SystemClock;
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_ext::McpSharing;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_prompt::SessionPrompt;
use rust_cli_kernel_api::store::Store;
use rust_cli_node_store::NodeStore;
use rust_cli_paths::StoragePaths;
use rust_cli_platform::local::McpTrustError;
use rust_cli_runtime::{
    CompactConfig, ContextConfig, Defaults, HooksConfig, LocalKernel, Ports, Surface, TitleModel,
};

use super::deployment::Traits;
use super::env_layout::PrimaryPaths;
use super::mcp::{McpFlags, Prewait};
use super::{Controller, Ported, env_assembly, limits, options, setup};
use crate::VERSION;
use crate::commands::args::cli_rules;
use crate::commands::hooks_trust;
use crate::config;
use crate::config::mcp_trust::{McpDecision, WorkspaceMcpTrust};
use crate::process::facts::Facts;

/// 一个工作区的内核、新会话的缺省模式与这个工作区的 MCP 信任（准入端口与待信任的提示，18 §12）。
pub(crate) struct WorkspaceKernel {
    pub(crate) kernel: Arc<LocalKernel>,
    pub(crate) mode: PermissionMode,
    pub(crate) mcp: Arc<WorkspaceMcpTrust>,
}

/// 组装工作区内核要的进程级部件（自己持有，组装可以在进程的任何时候发生）。
pub(crate) struct WorkspaceKernels {
    surface: Surface,
    sharing: McpSharing,
    /// 前端撑得起的 MCP 服务器询问（ext.md §3.13.1）。
    mcp_client: rust_cli_ext::McpClientTraits,
    env: EnvSnapshot,
    /// 读配置文件用的存储位置（与 [`super::open`] 读配置时的同一份）。
    config_paths: StoragePaths,
    traits: Traits,
    primary: MachineEnv,
    primary_paths: PrimaryPaths,
    /// controller 本机的文件（MCP 服务器绑定机器的旁路文件在这里）。
    controller: Arc<LocalEnvironment>,
    paths: StoragePaths,
    /// 会话库（本机设置经它读写；共享进程只接受本机库，见 `assemble::open`）。
    store: Arc<dyn Store>,
    /// 本机存储：任务清单总在这里（pg-store.md §6）。
    node: Arc<NodeStore>,
    clock: Arc<SystemClock>,
    monitor: bool,
    ports: Ports,
    /// 新会话的缺省（模式按工作区换）。
    defaults: Defaults,
    /// 定下缺省模型的任务（[`Ported::choosing`]）：只为持有，与 `Built::_choosing` 同理，丢掉作用域任务就停了。
    _choosing: TaskScope,
    compact: CompactConfig,
    prewait: Prewait,
    mcp_flags: McpFlags,
    /// 会话标题的生成器（规格开了 `titles` 才有）：每个工作区的内核都带上它，同 TUI 的进程级内核。
    titles: Option<TitleModel>,
    /// 新会话钉住的模型可见输入（context.md §15.3）：App 没有 `--system-prompt` 一类参数，只有 Host 声明的前端能力。
    prompt: SessionPrompt,
    /// App 前端层（context.md §15.5）：每个工作区的内核按它的插件集装一份端口。
    app: crate::frontends::app::AppFrontend,
    /// 进程级的授权登记（ext.md §3.12.14）：每个工作区的内核共用这一份，授权之后全部内核的连接一起重连。
    mcp_sign_ins: Option<Arc<rust_cli_ext::McpSignIns>>,
}

impl WorkspaceKernels {
    pub(crate) fn new(
        spec: &Controller,
        facts: &Facts,
        (config_paths, clock): (&StoragePaths, &Arc<SystemClock>),
        ported: Ported,
    ) -> Self {
        let Ported {
            opened,
            ports,
            selection,
            choosing,
            limits: model_limits,
        } = ported;
        let env = &facts.process.env;
        let cli = &spec.cli;
        let titles = spec.titles.then(|| TitleModel {
            model: ports.model.clone(),
            directory: opened.directory.clone(),
        });
        Self {
            surface: spec.surface,
            sharing: spec.mcp_sharing(),
            mcp_client: spec.mcp_client,
            env: env.clone(),
            config_paths: config_paths.clone(),
            traits: opened.traits,
            primary: opened.primary,
            primary_paths: opened.primary_paths,
            controller: opened.env,
            paths: opened.paths,
            store: opened.store,
            node: opened.node,
            clock: clock.clone(),
            monitor: opened.monitor,
            ports,
            defaults: super::defaults(spec, facts, &selection, PermissionMode::Default),
            _choosing: choosing,
            compact: options::compact(env, facts.controller.max_output_tokens, model_limits),
            prewait: Prewait {
                streaming_input: cli.streaming_input,
                tool_search: limits::tool_search(env).allows(),
            },
            mcp_flags: cli.mcp.clone(),
            titles,
            prompt: SessionPrompt::new(None, None, spec.frontend.clone()),
            app: spec.app.clone(),
            mcp_sign_ins: opened.mcp_sign_ins,
        }
    }

    /// 停止或失败之后扣住排队的输入（session-lifecycle.md §10.6，02 §11 C2）：只有 v5 前端的组装打开（protocol-v5.md §15.5）；
    /// TUI、`-p` 与 v4 的 App 不调它，行为不变。
    pub(crate) fn holding_queue(mut self) -> Self {
        self.defaults.limits.hold_queue = true;
        self
    }

    /// 会话主 env：v5 的读面读本机附件经它的文件端口（protocol-v5.md §41.4；SSH 部署时是那台机器）。
    pub(crate) fn primary(&self) -> &MachineEnv {
        &self.primary
    }

    /// 新会话的模型（进程级），给协议前端的缺省用。
    /// 合并 feat/rust-cli 时改：缺省模型变成可以晚到的值（performance.md §12.3）。App 的 `Controller::app` 是
    /// `wait_model: true`，组装时已等它定下，所以这里取现值就是定下的值。
    pub(crate) fn selection(&self) -> ModelSelection {
        self.defaults.selection.current()
    }

    /// 读工作区的配置用的输入（组装与 `mcp.trust.set` 的锁里重读是同一组）。
    fn config_inputs<'a>(&'a self, workspace: &'a WorkspaceRef) -> config::ConfigInputs<'a> {
        config::ConfigInputs {
            files: self.primary.files().as_ref(),
            workspace,
            paths: &self.config_paths,
            process_env: &self.env,
            flag: None,
            project_is_local: self.traits.project_config_local,
            shared_process: true,
        }
    }

    /// 组装工作区 `root` 的内核：读它的配置（用户层 + 项目层；项目级的 `storage.*` 不读），建内核、换上 hooks 与权限
    /// 规则，开始连接内核级的 MCP 服务器、加载插件（不等）。项目级 MCP 服务器连接之前按信任记录准入（18 §12.4）。
    pub(crate) async fn build(&self, root: &str) -> WorkspaceKernel {
        let workspace = env_assembly::workspace_at(root);
        let config = config::assemble(&self.config_inputs(&workspace)).await;
        // 与要连的服务器同一次读到的声明与 digest：准入按这份快照（审核、digest、执行用同一份，18 §12.4）
        let mcp = Arc::new(config.mcp_trust.clone());
        let mode = config::initial_mode(None, config.mode, self.store.as_ref(), &workspace).await;
        let mut context = ContextConfig::new(self.surface);
        let layout = env_assembly::layout(self.traits, &self.primary_paths);
        context.memory_dir =
            options::memory_dir(&self.primary, &layout, &self.paths, &workspace, &self.env).await;
        let mut extensions = setup::extensions(
            (&self.primary, &workspace, &self.primary_paths),
            self.controller.as_ref(),
            &self.env,
            (&self.mcp_flags, &[], self.prewait),
            (&[], &self.app, &self.paths.mcp_machines()),
            (&config, &layout, (self.sharing, self.mcp_client)),
        )
        .await;
        extensions.monitor = self.monitor;
        extensions.mcp_admission = Some(mcp.clone());
        extensions.mcp_sign_ins = self.mcp_sign_ins.clone();
        extensions.cron_store =
            env_assembly::cron_store(self.traits, &workspace, self.clock.clone());
        let defaults = Defaults {
            mode,
            ..self.defaults.clone()
        };
        let kernel = LocalKernel::with_extensions(
            self.ports.clone(),
            defaults,
            context,
            self.compact.clone(),
            self.node.clone(),
            extensions,
        )
        .with_prompt(self.prompt.clone());
        // 修 bug（titles.md §1）：App 的会话都开在这里组装的内核上，以前只有进程级的内核按规格带标题生成器，
        // 打开 `Controller::app` 的 `titles` 之后 App 的会话照样不起标题
        let kernel = Arc::new(match &self.titles {
            Some(titles) => kernel.with_titles(titles.clone()),
            None => kernel,
        });
        kernel.replace_hooks(config.hooks.clone());
        // 只取规则：记住的附加工作目录 App 不读（附加目录的 owner 是 Host，permission-updates.md §6.2）。
        let local = config::local_rules(self.store.as_ref(), &workspace).await;
        kernel.replace_permission_rules(
            config.permission_rules(local.sourced(), cli_rules(&[], &[])),
        );
        kernel.start_extensions();
        tracing::info!(
            event = "app.workspace.assembled",
            workspace = root,
            mode = mode.node_name(),
            "assembled a workspace kernel"
        );
        WorkspaceKernel { kernel, mode, mcp }
    }

    /// `hooks.trustGrant`（config.md §4.5）：`zcode hooks trust grant` 的同一段代码写这一条的预信任，工作区文件经主 env 的文件端口读
    /// （同组装）。拒绝时是公开的原因码。
    pub(crate) async fn pretrust_hooks(
        &self,
        root: &str,
        bundle: &str,
        digest: &str,
    ) -> Result<(), &'static str> {
        let context = hooks_trust::TrustContext {
            files: self.primary.files().as_ref(),
            paths: &self.config_paths,
            cwd: root,
            version: VERSION,
            now_ms: self.clock.now_ms(),
        };
        hooks_trust::pretrust(&context, bundle, digest).await
    }

    /// 重读这个工作区的 hooks（config.md §4.5）：§3 的同一次组装，只取 hooks（用户级与项目级的声明、现在的信任记录）。
    pub(crate) async fn hooks(&self, root: &str) -> HooksConfig {
        let workspace = env_assembly::workspace_at(root);
        config::assemble(&self.config_inputs(&workspace))
            .await
            .hooks
    }

    /// `mcp.trust.set`（18 §12.3）：记录文件的锁里按磁盘上的声明重验，再写记录。返回有没有改文件。
    pub(crate) async fn trust_mcp(
        &self,
        root: &str,
        decision: &McpDecision<'_>,
    ) -> Result<bool, McpTrustError> {
        let workspace = env_assembly::workspace_at(root);
        let granted_at = rust_cli_base::js::iso_timestamp(self.clock.now_ms());
        config::mcp_trust::decide(&self.config_inputs(&workspace), decision, &granted_at).await
    }
}
