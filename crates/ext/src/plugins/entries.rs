//! 插件引用目录的条目（docs/specs/rust-cli/ext.md §4.7）：App 的协议前端按它答输入框 @ 插件的查询。
//!
//! 取 [`PluginSet`]，不另读一套：加载好的插件（启用）按加载顺序，接着是判定时因「未启用」跳过的候选（停用）。别的跳过原因
//! （目录或 manifest 坏、隐藏的内置插件、重复的 id、同名、依赖 Node 宿主）不进目录：内核里没有它们，引用不到。

use serde_json::Value;

use super::{PluginSet, SkipReason};

/// 引用目录里的一个插件。skill 与 agent 的名字由 runtime 按那一刻的目录补上（它们要读执行环境）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// `<name>@<marketplace>`。
    pub id: String,
    pub name: String,
    pub marketplace: String,
    pub enabled: bool,
    /// manifest 的 `description`（去掉首尾空白，空的不算）；停用的没有（判定时没留下 manifest）。
    pub description: Option<String>,
    /// 这个插件的 MCP 服务器名（`plugin:<插件名>:<server>`）；停用的为空。
    pub mcp_servers: Vec<String>,
}

pub(super) fn entries(set: &PluginSet) -> Vec<PluginEntry> {
    let enabled = set.plugins().iter().map(|plugin| PluginEntry {
        id: plugin.id().to_owned(),
        name: plugin.name().to_owned(),
        marketplace: plugin.marketplace.clone(),
        enabled: true,
        description: plugin
            .manifest
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        mcp_servers: plugin
            .mcp_servers
            .iter()
            .map(|server| server.name.clone())
            .collect(),
    });
    let disabled = set
        .skipped()
        .iter()
        .filter(|skipped| skipped.reason == SkipReason::Disabled)
        .filter_map(|skipped| {
            // 「未启用」一定在 manifest 读出之后判，所以有 id；id 按最后一个 `@` 分成名字与 marketplace（名字里没有 `@`）
            let id = skipped.id.as_deref()?;
            let (name, marketplace) = id.rsplit_once('@')?;
            Some(PluginEntry {
                id: id.to_owned(),
                name: name.to_owned(),
                marketplace: marketplace.to_owned(),
                enabled: false,
                description: None,
                mcp_servers: Vec::new(),
            })
        });
    enabled.chain(disabled).collect()
}

#[cfg(test)]
#[path = "entries_tests.rs"]
mod tests;
