//! 工作区内核的缓存（工作线 F145，docs/specs/rust-cli/shared-workspaces.md §3；protocol-v5.md §9）：每个工作区第一次被用到
//! 时组装一套内核（`assemble::workspaces`），缓存到进程退出或释放；同一个工作区并发的组装只做一次（后到的等先到的结果）。
//! 两个协议前端共用这份缓存，各自实现自己的 `Workspaces` 端口：
//!
//! - protocol-zcode（v4）：[`HostedWorkspaces`]，进程 cwd 的那一套在启动时组装（`ready` 之前，与以前相同）；
//! - protocol-v5：[`V5Workspaces`]，没有进程 cwd 那一套（v5 没有不带工作区的会话请求），可以释放（`agent.release`）。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::default_model::unselected;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env::{EnvError, EnvPath, ReadFile};
use rust_cli_kernel_api::env_files::{FileKind, StatFile};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_platform::local::McpTrustError;
use rust_cli_protocol_v5::{
    AttachmentFile, AttachmentFileError, HookPretrust, HookPretrustRefusal, McpDecision,
    McpPending, McpTrustRefusal, McpTrusted, PluginReference,
};
use rust_cli_protocol_zcode::{Hosted, Workspaces};
use rust_cli_runtime::LocalKernel;
use tokio::sync::OnceCell;

use crate::assemble::env_assembly;
use crate::assemble::workspaces::{WorkspaceKernel, WorkspaceKernels};
use crate::config;
use crate::config::mcp_trust::WorkspaceMcpTrust;

/// 组装好的一套内核（缓存的值）。
#[derive(Clone)]
pub(crate) struct Built {
    pub(crate) kernel: Arc<LocalKernel>,
    pub(crate) mode: PermissionMode,
    /// 这个工作区的 MCP 信任（准入端口与待信任的提示，docs/zcode-protocol-v5/18-mcp-trust.md §12）。
    pub(crate) mcp: Arc<WorkspaceMcpTrust>,
}

impl From<WorkspaceKernel> for Built {
    fn from(built: WorkspaceKernel) -> Self {
        Self {
            kernel: built.kernel,
            mode: built.mode,
            mcp: built.mcp,
        }
    }
}

type Slot = Arc<OnceCell<Built>>;

/// 工作区路径 → 那一套内核（组装中的也在表里：同一个工作区只组装一次）。
pub(crate) struct KernelCache {
    kernels: WorkspaceKernels,
    table: Mutex<HashMap<String, Slot>>,
}

impl KernelCache {
    pub(crate) fn new(kernels: WorkspaceKernels) -> Self {
        Self {
            kernels,
            table: Mutex::new(HashMap::new()),
        }
    }

    fn slot(&self, root: &str) -> Slot {
        self.table
            .lock_unpoisoned()
            .entry(root.to_owned())
            .or_default()
            .clone()
    }

    /// 组装（或取缓存的）`root` 的内核。
    pub(crate) async fn open(&self, root: &str) -> Built {
        self.settle(root, self.slot(root)).await
    }

    /// 槽里的那一套：组装中就等它完成（同一个工作区只组装一次）。
    async fn settle(&self, root: &str, slot: Slot) -> Built {
        slot.get_or_init(|| async { Built::from(self.kernels.build(root).await) })
            .await
            .clone()
    }

    /// 表里有的那一套：组装中就等它完成；不在表里时为 None，不组装（`hooks.trustGrant` 之后重读用，config.md §4.5）。
    /// 组装在写记录之前读过记录的，等它完成再重读，新的信任就不会丢（17 §19.6）。
    pub(crate) async fn settled(&self, root: &str) -> Option<Built> {
        let slot = self.table.lock_unpoisoned().get(root).cloned()?;
        Some(self.settle(root, slot).await)
    }

    /// 已经组装好的；还没有或正在组装时为 None。
    pub(crate) fn existing(&self, root: &str) -> Option<Built> {
        let slot = self.table.lock_unpoisoned().get(root).cloned()?;
        slot.get().cloned()
    }

    /// 从缓存里拿走（之后第一次用到时重新组装）。正在进行的组装完成后结果随旧的槽丢掉。没有时返回假。
    pub(crate) fn release(&self, root: &str) -> bool {
        self.table.lock_unpoisoned().remove(root).is_some()
    }

    /// 组装用的进程级部件（`mcp.trust.set` 在锁里重读这个工作区的配置要它们，18 §12.3）。
    pub(crate) fn kernels(&self) -> &WorkspaceKernels {
        &self.kernels
    }

    /// 插件改了之后（docs/zcode-protocol-v5/24-plugins.md §13.2）：受影响的工作区从缓存里拿走，下一次用到时按新的插件重新组装；
    /// 开着的会话的通道仍拿着打开它的那一套（protocol-v5.md §72.9）。正在组装的随旧的槽丢掉（同 [`Self::release`]）。返回拿走的工作区。
    pub(crate) fn plugins_changed(&self, changed: &PluginsChanged) -> Vec<String> {
        let mut table = self.table.lock_unpoisoned();
        let gone: Vec<String> = table
            .keys()
            .filter(|root| changed.affects(root))
            .cloned()
            .collect();
        for root in &gone {
            table.remove(root);
        }
        gone
    }
}

/// 插件的哪一层改了（24 §13.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PluginsChanged {
    /// 用户层、已装的插件：每个工作区都受影响。
    User,
    /// 这个工作区的项目层：它与根在它下面的工作区（项目层自根向工作区根合并，config.md §2.1）。
    Workspace(String),
}

impl PluginsChanged {
    fn affects(&self, root: &str) -> bool {
        match self {
            Self::User => true,
            Self::Workspace(changed) => std::path::Path::new(root).starts_with(changed.as_str()),
        }
    }
}

fn hosted(built: Built) -> Hosted {
    Hosted {
        kernel: built.kernel,
        mode: built.mode,
    }
}

/// protocol-zcode（v4）的 `Workspaces`：进程 cwd 的那一套在启动时组装，之后按需组装别的工作区。
pub(crate) struct HostedWorkspaces {
    cache: KernelCache,
    process: Hosted,
}

impl HostedWorkspaces {
    pub(crate) async fn start(kernels: WorkspaceKernels, cwd: &str) -> Self {
        let cache = KernelCache::new(kernels);
        let process = hosted(cache.open(cwd).await);
        Self { cache, process }
    }
}

#[async_trait]
impl Workspaces for HostedWorkspaces {
    async fn open(&self, root: &str) -> Result<Hosted, String> {
        Ok(hosted(self.cache.open(root).await))
    }

    fn existing(&self, root: &str) -> Option<Hosted> {
        self.cache.existing(root).map(hosted)
    }

    fn process(&self) -> Hosted {
        self.process.clone()
    }
}

/// protocol-v5 的 `Workspaces`：按需组装、可以释放（01 §17.1 的 `agent.release`）。缓存与插件管理的端口共用（插件改了之后拿走
/// 受影响的内核，24 §13.2）。
pub(crate) struct V5Workspaces {
    cache: Arc<KernelCache>,
    /// 模型目录（显式覆盖时没有）：新会话的缺省在建会话那一刻按它取。
    directory: Option<Arc<dyn ModelDirectory>>,
    /// 显式覆盖的模型（没有目录时新会话用它；组装时已定下）。
    fixed: ModelSelection,
}

impl V5Workspaces {
    pub(crate) fn new(
        kernels: WorkspaceKernels,
        directory: Option<Arc<dyn ModelDirectory>>,
    ) -> Self {
        Self {
            fixed: kernels.selection(),
            directory,
            cache: Arc::new(KernelCache::new(kernels)),
        }
    }

    /// 内核缓存（插件管理的端口用，24 §13.2）。
    pub(crate) fn cache(&self) -> Arc<KernelCache> {
        self.cache.clone()
    }

    /// 新会话的模型（22 §3.5 第 6 项）：有目录时是这一刻目录的缺省（Host 的账号层后来变了也跟着变；`session.create` 在 Host 的屏障里，
    /// 所以已经是最新一份账号层），没有可用模型时是空选择；显式覆盖时是组装时定下的那一个。
    ///
    /// 缺陷修复（22 §2.3 第 1 条）：以前组装时取一次就不变，账号层晚到时新会话一直是启动那一刻的缺省（常常是空的）。
    async fn new_session_selection(&self) -> ModelSelection {
        match &self.directory {
            Some(directory) => directory.catalog().await.default.unwrap_or_else(unselected),
            None => self.fixed.clone(),
        }
    }
}

#[async_trait]
impl rust_cli_protocol_v5::Workspaces for V5Workspaces {
    /// 会话命令经它建会话、打开会话（protocol-v5.md §15.1）。
    async fn open(&self, key: &str) -> Result<rust_cli_protocol_v5::WorkspaceKernel, String> {
        let built = self.cache.open(key).await;
        Ok(rust_cli_protocol_v5::WorkspaceKernel {
            kernel: built.kernel,
            selection: self.new_session_selection().await,
            mode: built.mode,
        })
    }

    fn release(&self, key: &str) -> bool {
        self.cache.release(key)
    }

    /// 与组装这个工作区的内核时同一个工作区（本机环境 + 根目录）：node-store 冷读出的会话事实用同一个 `LOCAL_ENVIRONMENT`。
    fn workspace(&self, key: &str) -> WorkspaceRef {
        env_assembly::workspace_at(key)
    }

    /// 这个工作区的内核快照里等信任的项目级 MCP 声明（18 §12.5）；还没组装时没有。
    async fn mcp_pending(&self, key: &str) -> Vec<McpPending> {
        match self.cache.existing(key) {
            Some(built) => pending(&built).await,
            None => Vec::new(),
        }
    }

    /// `mcp.trust.set`（18 §12.3、§12.4）：锁里重验并写记录；信任时内核级的同名服务器补连一次（不等它连上）；再按写入之后的记录
    /// 给待信任的声明。还没组装的工作区只写记录（没有活会话要重算）。
    async fn trust_mcp(
        &self,
        key: &str,
        decision: McpDecision,
    ) -> Result<McpTrusted, McpTrustRefusal> {
        let request = config::mcp_trust::McpDecision {
            server: &decision.server,
            digest: &decision.digest,
            trusted: decision.trusted,
        };
        let changed = self
            .cache
            .kernels()
            .trust_mcp(key, &request)
            .await
            .map_err(|error| match error {
                McpTrustError::DeclarationChanged => McpTrustRefusal::DeclarationChanged,
                other => McpTrustRefusal::Unavailable(other.to_string()),
            })?;
        let Some(built) = self.cache.existing(key) else {
            return Ok(McpTrusted {
                changed,
                pending: Vec::new(),
            });
        };
        if decision.trusted {
            built.kernel.readmit_mcp(&decision.server).await;
        }
        Ok(McpTrusted {
            changed,
            pending: pending(&built).await,
        })
    }

    /// 插件引用目录（protocol-v5.md §59.1，ext.md §4.7）：这个工作区内核加载的插件（没组装就组装，与建会话同一条路）。
    async fn plugin_references(&self, key: &str) -> Vec<PluginReference> {
        let built = self.cache.open(key).await;
        built
            .kernel
            .plugin_references()
            .await
            .into_iter()
            .map(|reference| PluginReference {
                id: reference.entry.id,
                name: reference.entry.name,
                marketplace: reference.entry.marketplace,
                enabled: reference.entry.enabled,
                description: reference.entry.description,
                skills: reference.skills,
                mcp_servers: reference.entry.mcp_servers,
                subagents: reference.subagents,
            })
            .collect()
    }

    /// `hooks.trustGrant`（protocol-v5.md §59.2，config.md §4.5）：写这一条的预信任；接受了且这个工作区的内核在表里（组装中就等它），
    /// 重读 hooks、整份替换内核的快照（正在运行的 hook 事件用旧快照）。没在表里时只写记录，以后组装时读到。
    async fn pretrust_hooks(
        &self,
        key: &str,
        request: HookPretrust,
    ) -> Result<(), HookPretrustRefusal> {
        let kernels = self.cache.kernels();
        kernels
            .pretrust_hooks(key, &request.bundle, &request.digest)
            .await
            .map_err(HookPretrustRefusal::from_wire)?;
        if let Some(built) = self.cache.settled(key).await {
            built.kernel.replace_hooks(kernels.hooks(key).await);
        }
        tracing::info!(
            event = "app.workspace.hooks_pretrusted",
            workspace = key,
            "pretrusted a project hook declaration"
        );
        Ok(())
    }

    /// 读面 stat 一个本机附件（protocol-v5.md §41.4）：会话主 env 上的这个路径，只认普通文件。
    async fn stat_attachment(
        &self,
        key: &str,
        path: &str,
    ) -> Result<AttachmentFile, AttachmentFileError> {
        let request = StatFile {
            path: EnvPath::new(path),
        };
        let found = self
            .cache
            .kernels()
            .primary()
            .files()
            .stat(&env_assembly::workspace_at(key), request)
            .await
            .map_err(|error| AttachmentFileError::Failed(error.to_string()))?;
        match found {
            Some(stat) if stat.kind == FileKind::File => Ok(AttachmentFile {
                size: stat.size,
                mtime_ms: stat.mtime_ms,
            }),
            _ => Err(AttachmentFileError::NotFound),
        }
    }

    /// 读面读一个本机附件的全部字节（§41.4）：先 stat（不是普通文件、超过上限的不读），读出之后再按读到的大小核一次
    /// （stat 之后文件可能变大，同 Node 以读取端的上限为准）。
    async fn read_attachment(
        &self,
        key: &str,
        path: &str,
        limit: u64,
    ) -> Result<Vec<u8>, AttachmentFileError> {
        let file = self.stat_attachment(key, path).await?;
        let too_large = |size| AttachmentFileError::TooLarge { size };
        if file.size > limit {
            return Err(too_large(file.size));
        }
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let read = self
            .cache
            .kernels()
            .primary()
            .files()
            .read_file(&env_assembly::workspace_at(key), request)
            .await
            .map_err(|error| match error {
                EnvError::NotFound { .. } => AttachmentFileError::NotFound,
                other => AttachmentFileError::Failed(other.to_string()),
            })?;
        let size = read.bytes.len() as u64;
        if size > limit {
            return Err(too_large(size));
        }
        Ok(read.bytes)
    }
}

/// 内核快照里等信任的声明（名字与 digest）换成 v5 端口的形状。
async fn pending(built: &Built) -> Vec<McpPending> {
    built
        .mcp
        .pending()
        .await
        .into_iter()
        .map(|(server, digest)| McpPending { server, digest })
        .collect()
}

#[cfg(test)]
#[path = "app_workspaces_tests.rs"]
mod tests;
