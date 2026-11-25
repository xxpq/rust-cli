//! `plugins.referenceCatalog*`（protocol-v5.md §59.1）：校验与错误码、结果的形状（authority、category、没有市场字段）、读在任务里（不挡链路）。
//! 插件从哪来由真实进程测（一致性场景 `workspace-plugin-reference-catalog`）。
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryStore;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::mcp_trust::{McpDecision, McpPending, McpTrustRefusal, McpTrusted};
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::{Harness, initialize};

#[cfg(not(windows))]
const KEY: &str = "/Users/dev/repo";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\repo";

/// 入口的替身：一个启用、一个停用的插件；有闸门时先等它打开（替身里的“工作区还在组装”）。
struct Catalog {
    gate: Option<Notify>,
}

#[async_trait]
impl Workspaces for Catalog {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        Ok(crate::test_link::kernel())
    }

    fn release(&self, _key: &str) -> bool {
        false
    }

    fn workspace(&self, key: &str) -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(key),
        }
    }

    async fn mcp_pending(&self, _key: &str) -> Vec<McpPending> {
        Vec::new()
    }

    async fn trust_mcp(
        &self,
        _key: &str,
        _decision: McpDecision,
    ) -> Result<McpTrusted, McpTrustRefusal> {
        Err(McpTrustRefusal::DeclarationChanged)
    }

    async fn plugin_references(&self, _key: &str) -> Vec<PluginReference> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        vec![
            PluginReference {
                id: "kit@inline".to_owned(),
                name: "kit".to_owned(),
                marketplace: "inline".to_owned(),
                enabled: true,
                description: Some("A kit".to_owned()),
                skills: vec!["kit:probe".to_owned()],
                mcp_servers: vec!["plugin:kit:main".to_owned()],
                subagents: vec!["kit:reviewer".to_owned()],
            },
            PluginReference {
                id: "off@inline".to_owned(),
                name: "off".to_owned(),
                marketplace: "inline".to_owned(),
                ..PluginReference::default()
            },
        ]
    }
}

fn query(id: u64, kind: &str, target: Value, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "query", "params": {
        "type": kind, "target": target, "params": params,
    }})
}

fn target() -> Value {
    json!({"hostId": "local", "workspaceKey": KEY})
}

async fn ready(gated: bool) -> (Harness, Arc<Catalog>) {
    let catalog = Arc::new(Catalog {
        gate: gated.then(Notify::new),
    });
    let mut h = Harness::new();
    h.handle.ready(
        catalog.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    (h, catalog)
}

async fn answer(message: Value) -> Value {
    let (mut h, _) = ready(false).await;
    h.send(message).await;
    h.reply(2, &mut Vec::new()).await
}

#[tokio::test]
async fn the_catalog_is_the_entry_s_plugins_in_the_contract_shape() {
    let reply = answer(query(2, REFERENCE_CATALOG, target(), json!({}))).await;
    assert_eq!(
        reply["result"],
        json!({"kind": "ok", "data": {"authority": "workspace", "plugins": [
            {"pluginId": "kit@inline", "name": "kit", "marketplace": "inline", "description": "A kit", "enabled": true,
             "conflictingPluginIds": [], "skillQualifiedNames": ["kit:probe"], "mcpServerNames": ["plugin:kit:main"],
             "subagentNames": ["kit:reviewer"]},
            {"pluginId": "off@inline", "name": "off", "marketplace": "inline", "enabled": false,
             "conflictingPluginIds": [], "skillQualifiedNames": [], "mcpServerNames": [], "subagentNames": []},
        ]}}),
        "{reply}"
    );
}

#[tokio::test]
async fn a_session_id_answers_with_the_session_authority_and_with_category_adds_other() {
    let session = answer(query(
        2,
        REFERENCE_CATALOG,
        target(),
        json!({"sessionId": "s1"}),
    ))
    .await;
    assert_eq!(
        session["result"]["data"]["authority"], "session",
        "{session}"
    );
    let category = answer(query(
        2,
        REFERENCE_CATALOG_WITH_CATEGORY,
        target(),
        json!({}),
    ))
    .await;
    let categories: Vec<Value> = category["result"]["data"]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plugin| plugin["category"].clone())
        .collect();
    assert_eq!(categories, [json!("other"), json!("other")], "{category}");
}

#[tokio::test]
async fn the_query_is_checked_and_needs_the_process() {
    let missing = answer(query(
        2,
        REFERENCE_CATALOG,
        json!({"hostId": "local"}),
        json!({}),
    ))
    .await;
    assert_eq!(
        missing["error"]["data"]["code"], "precondition.workspaceRequired",
        "{missing}"
    );
    let relative = answer(query(
        2,
        REFERENCE_CATALOG,
        json!({"hostId": "local", "workspaceKey": "repo"}),
        json!({}),
    ))
    .await;
    assert_eq!(
        relative["error"]["data"]["code"], "precondition.invalid",
        "{relative}"
    );
    let bad = answer(query(
        2,
        REFERENCE_CATALOG,
        target(),
        json!({"sessionId": ""}),
    ))
    .await;
    assert_eq!(
        bad["error"]["data"]["code"], "precondition.invalidPayload",
        "{bad}"
    );
    let mut h = Harness::new();
    h.send(initialize()).await;
    h.next().await;
    h.send(query(2, REFERENCE_CATALOG, target(), json!({})))
        .await;
    let early = h.reply(2, &mut Vec::new()).await;
    assert_eq!(
        early["error"]["data"]["code"], "precondition.storeUnavailable",
        "{early}"
    );
}

/// 工作区还在组装时 actor 照常处理别的请求（读在任务里，§59.1）。
#[tokio::test]
async fn a_slow_catalog_does_not_hold_the_link() {
    let (mut h, catalog) = ready(true).await;
    h.send(query(2, REFERENCE_CATALOG, target(), json!({})))
        .await;
    h.send(json!({"jsonrpc": "2.0", "id": 3, "method": "ping", "params": {}}))
        .await;
    let first = h.next().await;
    assert_eq!(first["id"], 3, "{first}");
    catalog.gate.as_ref().unwrap().notify_one();
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(reply["result"]["kind"], "ok", "{reply}");
}
