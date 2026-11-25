//! 对话引用的插件目录（protocol-v5.md §59.1；docs/zcode-protocol-v5/17-host-seam.md §19.3）：查询 `plugins.referenceCatalog` 与
//! `plugins.referenceCatalogWithCategory`。在任务里经 [`Workspaces::plugin_references`] 取这个工作区内核加载的插件（组装中就等它，
//! 没有就组装），回复走 [`crate::server::Event::Queried`]（与 `sessions.facts` 同一条回复路径与在途计数）：actor 不等工作区组装。
//!
//! 带不带 `sessionId` 答的是同一份：rust 的会话都用工作区内核的插件，插件只在内核组装时加载一次（ext.md §4.7）；带了 `authority` 写 `session`。

use std::sync::{Arc, OnceLock};

use rust_cli_protocol_core::schema::Node;
use serde_json::Value;

use crate::commands::{command_schema, field_path};
use crate::errors::{Fault, protocol_error};
use crate::generated::catalog_plugins_reference_v1::{
    QueryPluginsReferenceCatalogParams, QueryPluginsReferenceCatalogResult,
    QueryPluginsReferenceCatalogResultAuthority, QueryPluginsReferenceCatalogResultPluginsItem,
};
use crate::generated::contract::codes;
use crate::generated::core_v1::{QueryEnvelope, QueryResult, QueryResultOk};
use crate::server::{Actor, Workspaces};
use crate::topics;

/// 查询类型（catalog.plugins.reference@1）。
pub(crate) const REFERENCE_CATALOG: &str = "plugins.referenceCatalog";
pub(crate) const REFERENCE_CATALOG_WITH_CATEGORY: &str = "plugins.referenceCatalogWithCategory";
/// 两个查询共用的参数 schema 名（契约里同一个 `referenceCatalogQuery`）。
const PARAMS_SCHEMA: &str = "Query.plugins.referenceCatalog.params";
const SCHEMA: &str = include_str!("../schema/catalog.plugins.reference@1.json");
/// 没有市场 listing 时的分类：rust 不读 listing，同 Node 读不到 listing 时的口径（17 §19.3）。
const UNLISTED_CATEGORY: &str = "other";

/// 目录里的一个插件（入口按工作区内核加载的插件给，ext.md §4.7）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginReference {
    /// `<name>@<marketplace>`。
    pub id: String,
    pub name: String,
    pub marketplace: String,
    pub enabled: bool,
    /// manifest 的描述。
    pub description: Option<String>,
    /// Skill 工具用的名字（`<插件名>:<目录名>`）。
    pub skills: Vec<String>,
    /// MCP 服务器名（`plugin:<插件名>:<server>`）。
    pub mcp_servers: Vec<String>,
    /// Agent 工具用的类型名（`<插件名>:…`）。
    pub subagents: Vec<String>,
}

pub(crate) fn is_reference_query(kind: &str) -> bool {
    kind == REFERENCE_CATALOG || kind == REFERENCE_CATALOG_WITH_CATEGORY
}

fn params_schema() -> Option<&'static Node> {
    static NODE: OnceLock<Option<Node>> = OnceLock::new();
    command_schema(&NODE, SCHEMA, PARAMS_SCHEMA)
}

fn fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

fn item(
    reference: PluginReference,
    category: bool,
) -> QueryPluginsReferenceCatalogResultPluginsItem {
    QueryPluginsReferenceCatalogResultPluginsItem {
        category: category.then(|| UNLISTED_CATEGORY.to_owned()),
        plugin_id: reference.id,
        name: reference.name,
        marketplace: reference.marketplace,
        icon: None,
        display_name: None,
        display_name_i18n: None,
        description: reference.description,
        description_i18n: None,
        enabled: reference.enabled,
        // rust 只加载同名插件里的第一个（ext.md D22），目录里没有同名的两个
        conflicting_plugin_ids: Vec::new(),
        skill_qualified_names: reference.skills,
        mcp_server_names: reference.mcp_servers,
        subagent_names: reference.subagents,
    }
}

/// 校验（目标、参数）在任务里做、错误也走同一条回复路径（同 `sessions.facts`）。
async fn answer(
    workspaces: Option<Arc<dyn Workspaces>>,
    query: QueryEnvelope,
) -> Result<Value, Fault> {
    let Some(key) = query.target.workspace_key.as_deref() else {
        return Err(fault(
            codes::PRECONDITION_WORKSPACE_REQUIRED,
            format!("{} needs target.workspaceKey", query.r#type),
        ));
    };
    topics::local_workspace(key).map_err(|message| fault(codes::PRECONDITION_INVALID, message))?;
    let params = query
        .params
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if let Some(schema) = params_schema()
        && let Err(error) = schema.validate(&params, "")
    {
        let (field, message) = field_path(&error);
        return Err(fault(
            codes::PRECONDITION_INVALID_PAYLOAD,
            format!("{field}: {message}"),
        ));
    }
    let params: QueryPluginsReferenceCatalogParams = serde_json::from_value(params)
        .map_err(|error| fault(codes::PRECONDITION_INVALID_PAYLOAD, error.to_string()))?;
    let Some(workspaces) = workspaces else {
        return Err(fault(
            codes::PRECONDITION_STORE_UNAVAILABLE,
            "the process is not ready yet",
        ));
    };
    let category = query.r#type == REFERENCE_CATALOG_WITH_CATEGORY;
    let result = QueryPluginsReferenceCatalogResult {
        authority: if params.session_id.is_some() {
            QueryPluginsReferenceCatalogResultAuthority::Session
        } else {
            QueryPluginsReferenceCatalogResultAuthority::Workspace
        },
        plugins: workspaces
            .plugin_references(key)
            .await
            .into_iter()
            .map(|reference| item(reference, category))
            .collect(),
    };
    // 生成的结构体总是序列化成对象（同本 crate 别处的 `unwrap_or`）
    let data = serde_json::to_value(result).unwrap_or(Value::Null);
    Ok(
        serde_json::to_value(QueryResult::Ok(QueryResultOk { data, cursor: None }))
            .unwrap_or(Value::Null),
    )
}

impl Actor {
    /// `plugins.referenceCatalog*`（§59.1）：交给任务，回复等它。
    pub(crate) fn plugin_references(&mut self, reply: &Value, query: QueryEnvelope) {
        let workspaces = self.sources().map(|(workspaces, _)| workspaces);
        self.answer_in_task(
            "protocol-v5-plugin-references",
            reply,
            answer(workspaces, query),
        );
    }
}

#[cfg(test)]
#[path = "plugin_references_tests.rs"]
mod tests;
