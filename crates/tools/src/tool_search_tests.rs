use std::sync::Arc;

use rust_cli_base::models::{DEEPSEEK_V4_PRO, GLM_5_3, HAIKU_3_5_DATED};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::listing::ListingSource;
use rust_cli_kernel_api::mcp::McpToolInfo;
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{ContextRequest, ToolRegistry};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::run_tools::RunTools;
use rust_cli_kernel_api::testing::{ListTools, ScriptedInteractions};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::plan_mode::EnterPlanModeTool;
use crate::test_more::{Ctx, output_text};

fn registry() -> Arc<dyn ToolRegistry> {
    Arc::new(ListTools(vec![
        Arc::new(crate::GlobTool::new()),
        Arc::new(ToolSearchTool::new(Default::default())),
        Arc::new(EnterPlanModeTool::new(true)),
        Arc::new(EnterPlanModeTool::new(false)),
    ]))
}

/// 主线程注册表与绑定到它的目录（延迟工具清单）。
fn catalog() -> (Arc<ToolCatalog>, Arc<dyn ToolRegistry>) {
    let catalog = Arc::new(ToolCatalog::default());
    let registry = registry();
    catalog.bind(&registry);
    (catalog, registry)
}

/// 在 `tools`（调用上下文里这次运行的工具）里搜。
async fn run_in(tools: Arc<dyn ToolRegistry>, query: &str) -> String {
    let tool = ToolSearchTool::new(Default::default());
    let (progress, _) = mpsc::channel(1);
    let context = ToolUseContext {
        tools: Arc::new(RunTools::new(tools, &[])),
        ..Ctx::new(Arc::new(ScriptedInteractions::default())).build()
    };
    let output = tool
        .call(json!({"query": query}), context, progress)
        .await
        .unwrap();
    output_text(&output)
}

async fn run(query: &str) -> String {
    run_in(registry(), query).await
}

#[tokio::test]
async fn select_returns_tool_references() {
    assert_eq!(run("select:EnterPlanMode").await, "<ref:EnterPlanMode>");
    // 已加载的工具也能选中（无害的空操作）；重复与空项去掉；找不到的忽略。
    assert_eq!(
        run("SELECT: Glob , EnterPlanMode,Glob,,Missing").await,
        "<ref:Glob>\n<ref:EnterPlanMode>"
    );
    assert_eq!(run("select:Missing").await, texts::NO_DEFERRED_MATCH);
    assert_eq!(run("plan mode").await, "<ref:EnterPlanMode>");
}

/// 只搜调用上下文里这次运行的工具（tools-more.md §3.5）：子代理的工具里没有 EnterPlanMode 时，`select:` 与关键词都找不到它，
/// 即使主线程的注册表里有。修前 ToolSearch 搜主线程注册表的视图，子代理里找得到 agent 定义不给的工具。
#[tokio::test]
async fn only_the_tools_of_this_run_are_searched() {
    let subagent: Arc<dyn ToolRegistry> = Arc::new(ListTools(vec![
        Arc::new(crate::GlobTool::new()),
        Arc::new(ToolSearchTool::new(Default::default())),
    ]));
    assert_eq!(
        run_in(subagent.clone(), "select:EnterPlanMode").await,
        texts::NO_DEFERRED_MATCH
    );
    assert_eq!(
        run_in(subagent, "plan mode").await,
        texts::NO_DEFERRED_MATCH
    );
}

fn request(model: &str, names: &[&str]) -> ContextRequest {
    ContextRequest {
        tool_names: names.iter().map(|name| (*name).to_owned()).collect(),
        ..ContextRequest::new(
            SessionId::new("sess_1"),
            WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/work"),
            },
            QuerySource::MainThread,
            PermissionMode::Default,
            ModelSelection::new("anthropic".to_owned(), model.to_owned()),
        )
    }
}

#[tokio::test]
async fn the_listing_names_deferred_tools_only_when_tool_search_is_on() {
    let (catalog, _registry) = catalog();
    let names = ["Glob", "ToolSearch", "EnterPlanMode"];
    let listings = catalog.listings(&request(GLM_5_3, &names)).await.unwrap();
    let deferred: Vec<&str> = listings
        .deferred_tools
        .iter()
        .map(|entry| entry.line.as_str())
        .collect();
    assert_eq!(deferred, ["EnterPlanMode"]);
    // 没有 ToolSearch：不宣布。
    let off = catalog
        .listings(&request(GLM_5_3, &["Glob", "EnterPlanMode"]))
        .await
        .unwrap();
    assert!(off.deferred_tools.is_empty());
    // 工作线 F89（tools-more.md §3.2.1）：不区分模型，以前禁用表里的模型也宣布。修改：F84 时不宣布。
    let legacy = catalog
        .listings(&request(HAIKU_3_5_DATED, &names))
        .await
        .unwrap();
    assert_eq!(legacy.deferred_tools.len(), 1);
    // 工作线 F84（tools-more.md §3.2）：端点不支持 `tool_reference` 时是本地 ToolSearch，清单照样宣布。修改：以前
    // 不宣布（没有卡片的模型、端点不支持的都不开）。
    let local = catalog
        .listings(&request(DEEPSEEK_V4_PRO, &names))
        .await
        .unwrap();
    let lines: Vec<&str> = local
        .deferred_tools
        .iter()
        .map(|entry| entry.line.as_str())
        .collect();
    assert_eq!(lines, ["EnterPlanMode"]);
}

/// 工作线 F89（tools-more.md §3.2.1）：`ENABLE_TOOL_SEARCH` 为关时 ToolSearch 工具不启用（这一步的工具里没有它），开与自动时
/// 启用（自动的门槛在主循环里按这一步判断）。
#[test]
fn the_setting_enables_the_tool_search_tool() {
    use rust_cli_prompts::models::ToolSearchSetting;
    for (setting, enabled) in [
        (ToolSearchSetting::On, true),
        (ToolSearchSetting::Auto { percent: 10 }, true),
        (ToolSearchSetting::Off, false),
    ] {
        let tool = ToolSearchTool::new(setting);
        assert_eq!(tool.is_enabled(), enabled, "{setting:?}");
    }
    assert!(ToolSearchTool::new(Default::default()).is_enabled());
}

#[test]
fn tool_search_itself_is_never_deferred() {
    let tool = ToolSearchTool::new(Default::default());
    assert!(!is_deferred(&tool));
}

/// 只用来占名字的延迟工具（`mcp` 有值时是 MCP 工具）。
struct Named {
    name: &'static str,
    mcp: Option<McpToolInfo>,
}

#[async_trait::async_trait]
impl Tool for Named {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        self.name
    }
    fn input_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        String::new()
    }
    fn should_defer(&self) -> bool {
        true
    }
    fn mcp_info(&self) -> Option<&McpToolInfo> {
        self.mcp.as_ref()
    }
    async fn call(
        &self,
        _input: serde_json::Value,
        _context: rust_cli_kernel_api::tool::ToolUseContext,
        _progress: mpsc::Sender<rust_cli_kernel_api::tool::ToolProgress>,
    ) -> Result<rust_cli_kernel_api::tool::ToolOutput, rust_cli_kernel_api::tool::ToolError> {
        Err(rust_cli_kernel_api::tool::ToolError::Unsupported {
            what: self.name.to_owned(),
        })
    }
}

/// 延迟工具清单按 UTF-16 码元序：大写开头的
/// 内置工具排在小写开头的 MCP 工具之前。缺陷修复前按不分大小写的顺序，MCP 工具夹在中间（R4 F1）。
#[tokio::test]
async fn the_deferred_listing_is_in_code_unit_order() {
    let catalog = Arc::new(ToolCatalog::default());
    let registry: Arc<dyn ToolRegistry> = Arc::new(ListTools(vec![
        Arc::new(ToolSearchTool::new(Default::default())),
        Arc::new(Named {
            name: "mcp__fixture__lookup",
            mcp: Some(McpToolInfo {
                server: "fixture".to_owned(),
                tool: "lookup".to_owned(),
                always_load: false,
            }),
        }),
        Arc::new(Named {
            name: "WebFetch",
            mcp: None,
        }),
        Arc::new(Named {
            name: "ListMcpResourcesTool",
            mcp: None,
        }),
    ]));
    catalog.bind(&registry);
    let names = [
        "ToolSearch",
        "mcp__fixture__lookup",
        "WebFetch",
        "ListMcpResourcesTool",
    ];
    let listings = catalog.listings(&request(GLM_5_3, &names)).await.unwrap();
    let deferred: Vec<&str> = listings
        .deferred_tools
        .iter()
        .map(|entry| entry.line.as_str())
        .collect();
    assert_eq!(
        deferred,
        ["ListMcpResourcesTool", "WebFetch", "mcp__fixture__lookup"]
    );
}

/// R1-13：没绑定注册表时延迟清单报错（原来静默当作没有工具：清单为空）。ToolSearch 不经目录（tools-more.md §3.5）。
#[tokio::test]
async fn an_unbound_catalog_fails_instead_of_listing_nothing() {
    let catalog = Arc::new(ToolCatalog::default());
    let listed = catalog
        .listings(&request(GLM_5_3, &["ToolSearch", "WebFetch"]))
        .await;
    assert!(listed.is_err());
}
