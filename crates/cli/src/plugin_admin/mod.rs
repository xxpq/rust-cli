//! 插件管理的服务（docs/specs/rust-cli/plugin-store.md §12）：列、装、卸、更新、启停、配置值、恢复内置、市场的加删刷新、
//! 孤儿清理。组合插件存储（plugin-store）、配置的持锁写者（platform）与加载（ext 的 `load`，列出用）。
//!
//! A1：这是 cli 里的具体类型，不进 kernel-api；前端（`zcode plugin`、以后的 TUI 插件页与 v5 的 `catalog.plugins@1`）各自定
//! 窄端口包一层。每个操作是 `async fn`，在调用方的任务里跑完才返回；同一个进程里并发调用安全：写的串行化是存储的合同
//! （进程内的门 + Node 的存储锁，§4）。写操作收一个 [`Control`]：取消在锁外取内容与等锁时生效，持锁提交之后不看。
//! 每个操作都重读配置与磁盘（不做文件监听，config.md D6）。

pub(crate) mod command;
mod facts;
mod listing;
pub(crate) mod output;
mod writes;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rust_cli_base::clock::SystemClock;
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_ext::plugins::Unhosted;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_net::Egress;
use rust_cli_paths::StoragePaths;
use rust_cli_plugin_store::local::{Gate, Layout, LocalStore};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::config::{self, ConfigInputs, ConfigSnapshot};

pub(crate) use facts::{ComponentKind, Layer, Origin, Package};
pub(crate) use listing::Listed;
pub(crate) use writes::{InstallOutcome, SetEnabled};

/// 写操作的阶段（§12）：锁外取内容 → 等存储锁 → 持锁提交。前两个可以取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Fetching,
    WaitingForLock,
    Committing,
}

/// 一个写操作的控制：取消令牌与当前阶段（v5 的 `operations` 由它映射，§12）。
pub(crate) struct Control {
    pub(crate) cancel: CancellationToken,
    pub(crate) phase: watch::Sender<Phase>,
}

impl Control {
    pub(crate) fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            phase: watch::channel(Phase::Fetching).0,
        }
    }

    fn enter(&self, phase: Phase) {
        self.phase.send_replace(phase);
    }
}

/// 写到哪一级配置（`-s user` / `-s project`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    User,
    /// 项目层（Node 内部叫 workspace）：`<cwd>/.zcode/config.json`。
    Workspace,
}

/// 插件管理的服务。
pub(crate) struct PluginAdmin {
    env: Arc<LocalEnvironment>,
    /// 本机的 home（市场来源里的 `~`）。
    home: PathBuf,
    paths: StoragePaths,
    process_env: EnvSnapshot,
    egress: Arc<Egress>,
    clock: Arc<SystemClock>,
    /// 同一个进程里所有插件写共用的门（§4）。
    gate: Gate,
    /// 前端撑不起来的插件（§12：`zcode plugin` 是不是 App 的那一份，App 按 Host 声明的能力）：列出时按它判加没加载。
    unhosted: Unhosted,
}

/// 一次操作读到的：这个工作区的配置快照与插件存储。
struct Context {
    config: ConfigSnapshot,
    store: LocalStore,
    workspace: WorkspaceRef,
    cwd: String,
}

/// 项目层的插件配置写哪个文件（Node `resolvePluginConfigPath`：固定写当前工作区的 `.zcode/config.json`）。
const PROJECT_CONFIG: [&str; 2] = [rust_cli_base::names::DOT_DIR, "config.json"];

impl PluginAdmin {
    pub(crate) fn new(
        (env, home): (Arc<LocalEnvironment>, PathBuf),
        paths: StoragePaths,
        process_env: EnvSnapshot,
        egress: Arc<Egress>,
        (clock, unhosted): (Arc<SystemClock>, Unhosted),
    ) -> Self {
        Self {
            env,
            home,
            paths,
            process_env,
            egress,
            clock,
            gate: Gate::new(),
            unhosted,
        }
    }

    /// 本机的 home（用户级的读以它为工作区，plugin-store.md §12.1）。
    pub(crate) fn home(&self) -> String {
        self.home.display().to_string()
    }

    fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// 读这个工作区的配置快照（插件存储根跟随合并后的 `storage.dir`，config.md §2.1）与存储。
    async fn context(&self, cwd: &str) -> Context {
        let workspace = WorkspaceRef {
            environment: EnvironmentId::new(rust_cli_node_store::LOCAL_ENVIRONMENT),
            root: EnvPath::new(cwd),
        };
        let inputs = ConfigInputs {
            files: self.env.as_ref(),
            workspace: &workspace,
            paths: &self.paths,
            process_env: &self.process_env,
            flag: None,
            project_is_local: true,
            shared_process: false,
        };
        let config = config::assemble(&inputs).await;
        let base = Path::new(cwd);
        let layout = Layout::new(
            &config.plugins.root,
            &self
                .paths
                .plugin_secrets(config.storage.dir.as_deref(), base),
        );
        let store = LocalStore::new(layout, self.egress.clone(), self.gate.clone());
        Context {
            config,
            store,
            workspace,
            cwd: cwd.to_owned(),
        }
    }

    /// 配置文件：用户级或项目层。
    fn config_file(&self, scope: Scope, cwd: &str) -> PathBuf {
        match scope {
            Scope::User => self.paths.cli_config().as_path().to_path_buf(),
            Scope::Workspace => PROJECT_CONFIG
                .iter()
                .fold(PathBuf::from(cwd), |path, part| path.join(part)),
        }
    }
}
