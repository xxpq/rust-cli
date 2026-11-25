//! 插件管理的端口（docs/specs/rust-cli/protocol-v5.md §72.1；docs/zcode-protocol-v5/24-plugins.md）：同 [`crate::Workspaces`]，
//! 由 protocol-v5 自己定义、入口实现。前端只经它读写：读每次读整份（不缓存），写进本链路唯一的写任务、按到达顺序做。
//!
//! 数据类型用契约生成的类型（`PluginFields` 等、命令参数 `CommandPlugins*`）：入口从插件存储的事实直接填它们，只换算一次。
//! 入口交了它，前端才声明 `catalog.plugins`（握手）。

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::generated::catalog_plugins_v1::{
    CommandPluginsConfigure, CommandPluginsInstall, CommandPluginsMarketplaceAdd,
    CommandPluginsMarketplaceRemove, CommandPluginsMarketplaceUpdate, CommandPluginsSetEnabled,
    CommandPluginsUninstall, CommandPluginsUpdate, PluginCatalogEntryFields, PluginDiagnostic,
    PluginFields, PluginMarketplaceFields, PluginWorkspaceFields, QueryPluginsDescribeResult,
};

/// 用户级的事实（topic `catalog/plugins` 的内容，操作表除外）。键是实体 id。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginInventory {
    /// 已装与内置的插件（id = `<name>@<marketplace>`）。
    pub plugins: BTreeMap<String, PluginFields>,
    /// 已加的市场（id = 市场名）。
    pub marketplaces: BTreeMap<String, PluginMarketplaceFields>,
    /// 市场目录里的条目（id = `<name>@<marketplace>`）。
    pub catalog: BTreeMap<String, PluginCatalogEntryFields>,
    /// 读不出的市场、坏的记录：读不失败，问题进这里（同 Node 的总览）。
    pub diagnostics: Vec<PluginDiagnostic>,
}

/// 一个工作区的事实（topic `ws/<key>/catalog/plugins` 的内容）：id 集合同用户级的 `plugins`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspacePlugins {
    pub plugins: BTreeMap<String, PluginWorkspaceFields>,
    /// 这个工作区跳过加载的插件与原因。
    pub diagnostics: Vec<PluginDiagnostic>,
}

/// 一条写操作（24 §4 的 8 条写命令）。`workspace` 是 `scope: workspace` 时的工作区（命令的 `target.workspaceKey`）。
#[derive(Debug, Clone, PartialEq)]
pub enum PluginOp {
    Install(CommandPluginsInstall),
    Uninstall(CommandPluginsUninstall),
    Update(CommandPluginsUpdate),
    SetEnabled {
        workspace: Option<String>,
        change: CommandPluginsSetEnabled,
    },
    Configure {
        workspace: Option<String>,
        change: CommandPluginsConfigure,
    },
    MarketplaceAdd(CommandPluginsMarketplaceAdd),
    MarketplaceRemove(CommandPluginsMarketplaceRemove),
    MarketplaceUpdate(CommandPluginsMarketplaceUpdate),
}

/// 写操作做完了。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginApplied {
    /// 设值语义下什么也没变（装已装的、卸没装的……，24 §4）。
    pub noop: bool,
    /// 警告与部分失败（ACK 的 `result.diagnostics`）。
    pub diagnostics: Vec<PluginDiagnostic>,
}

/// 写操作或 describe 没做成（24 §4.1 的拒绝码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginRefusal {
    /// 没装、也不在任何市场目录里。
    PluginNotFound(String),
    MarketplaceNotFound(String),
    /// 配置键不在 `userConfig` 里，或值的类型不对。
    OptionInvalid {
        key: String,
        message: String,
    },
    /// 提交点之前被取消。
    Cancelled,
    /// 别的失败（下载、校验、git、锁、写盘）。
    Failed(String),
}

#[async_trait]
pub trait PluginAdmin: Send + Sync {
    /// 用户级的事实：读整份，不缓存。
    async fn inventory(&self) -> PluginInventory;
    /// 一个工作区的事实：用户级每个插件在这个工作区的一层与生效的启用。
    async fn workspace(&self, key: &str) -> WorkspacePlugins;
    /// 一条写操作。`cancel` 在提交点之前被触发 → `Err(PluginRefusal::Cancelled)`；提交点之后不看它。
    async fn apply(
        &self,
        op: PluginOp,
        cancel: CancellationToken,
    ) -> Result<PluginApplied, PluginRefusal>;
    /// `plugins.describe`：只读本机已有的内容，不联网、不写盘。
    async fn describe(&self, plugin_id: &str) -> Result<QueryPluginsDescribeResult, PluginRefusal>;
}
