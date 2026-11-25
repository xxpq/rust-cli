//! 插件引用目录（docs/specs/rust-cli/ext.md §4.7；App 的协议前端按它答输入框 @ 插件的查询）：这套内核的 `PluginSet` 的条目，
//! 补上这一刻工作区 skill 与 agent 目录里来自各插件的名字（Skill 工具、Agent 工具用的名字）。

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_agents::{AgentCatalog, AgentDefinition, AgentSource};
use rust_cli_ext::{Ext, PluginEntry, Skill, SkillSource};
use rust_cli_kernel_api::listing::{ListingSource, Listings, PluginCapabilities};
use rust_cli_kernel_api::mcp_name;
use rust_cli_kernel_api::orchestration::{ContextRequest, OrchestrationError};

use crate::LocalKernel;

/// 引用目录里的一个插件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginReference {
    pub entry: PluginEntry,
    /// 来自这个插件的 skill（`<插件名>:<目录名>`），排好序；停用的插件没有。
    pub skills: Vec<String>,
    /// 来自这个插件的 agent 类型（`<插件名>:…`），排好序；停用的插件没有。
    pub subagents: Vec<String>,
}

/// 按插件名分拣 skill 与 agent。agent 的类型名第一段是插件名（agents.md §3），插件名里没有 `:`。
fn build(
    entries: Vec<PluginEntry>,
    skills: &[Arc<Skill>],
    agents: &[AgentDefinition],
) -> Vec<PluginReference> {
    entries
        .into_iter()
        .map(|entry| {
            let skills: BTreeSet<String> = skills
                .iter()
                .filter(|skill| matches!(&skill.source, SkillSource::Plugin { plugin } if *plugin == entry.name))
                .map(|skill| skill.name.clone())
                .collect();
            let subagents: BTreeSet<String> = agents
                .iter()
                .filter(|agent| agent.source == AgentSource::Plugin)
                .filter(|agent| agent.agent_type.split(':').next() == Some(entry.name.as_str()))
                .map(|agent| agent.agent_type.clone())
                .collect();
            PluginReference {
                entry,
                skills: skills.into_iter().collect(),
                subagents: subagents.into_iter().collect(),
            }
        })
        .collect()
}

impl LocalKernel {
    /// 这个内核的插件引用目录（ext.md §4.7）：插件在内核组装时加载成 `PluginSet`（ext.md §4.0），所以条目在内核的一生里不变；
    /// skill 与 agent 的名字每次按目录现读（同发模型请求前的清单）。没有工作区的内核（不加载插件）为空。
    pub async fn plugin_references(&self) -> Vec<PluginReference> {
        let Some(workspace) = self.ext.workspace() else {
            return Vec::new();
        };
        let skills = self.ext.skills().skills(workspace, &[]).await;
        let agents = self.agent_catalog.definitions(workspace, &[]).await;
        build(self.ext.plugins().entries(), &skills, &agents)
    }
}

/// `plugin_reference` 提醒的插件能力（docs/specs/rust-cli/context.md §15.11）：同一份引用目录，按这一步的工作区与附加目录读 skill
/// 与 agent；MCP 服务器只算这一步工具里有它的工具的（连着、模型看得到，同 Node 的「provider-visible」）。只给启用的。
pub(crate) struct PluginReferenceListings {
    pub(crate) ext: Arc<Ext>,
    pub(crate) agents: Arc<AgentCatalog>,
}

#[async_trait]
impl ListingSource for PluginReferenceListings {
    async fn listings(&self, _request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        Ok(Listings::default())
    }

    async fn plugin_references(
        &self,
        request: &ContextRequest,
        ids: &[String],
    ) -> Result<Vec<PluginCapabilities>, OrchestrationError> {
        let entries: Vec<PluginEntry> = self
            .ext
            .plugins()
            .entries()
            .into_iter()
            .filter(|entry| entry.enabled && ids.contains(&entry.id))
            .collect();
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let place = (
            &request.workspace,
            request.additional_directories.as_slice(),
        );
        let skills = self.ext.skills().skills(place.0, place.1).await;
        let agents = self.agents.definitions(place.0, place.1).await;
        Ok(capabilities(
            build(entries, &skills, &agents),
            &request.tool_names,
            ids,
        ))
    }
}

/// 引用目录的条目换成提醒用的能力：MCP 服务器只留这一步工具里有它的工具的（`tool_names` 是这一步的工具，模型看得到的），
/// 按引用的顺序排（context 按它用掉上限）。
fn capabilities(
    references: Vec<PluginReference>,
    tool_names: &[String],
    ids: &[String],
) -> Vec<PluginCapabilities> {
    let visible = |server: &str| {
        let prefix = mcp_name::server_prefix(server);
        tool_names.iter().any(|tool| tool.starts_with(&prefix))
    };
    let mut found: Vec<PluginCapabilities> = references
        .into_iter()
        .map(|reference| PluginCapabilities {
            mcp_servers: reference
                .entry
                .mcp_servers
                .iter()
                .filter(|server| visible(server))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            id: reference.entry.id,
            skills: reference.skills,
            subagents: reference.subagents,
        })
        .collect();
    found.sort_by_key(|capabilities| ids.iter().position(|id| *id == capabilities.id));
    found
}

#[cfg(test)]
#[path = "plugin_references_tests.rs"]
mod tests;
