//! ToolSearch 与延迟工具清单（docs/specs/rust-cli/tools-more.md §3）。
//!
//! ToolSearch 搜调用上下文里这次运行的工具（`ToolUseContext::tools`，tools-more.md §3.5）：主线程是顶层会话的视图去掉这一轮
//! 的禁用名单，子代理是按 agent 定义过滤的那一份。`ENABLE_TOOL_SEARCH` 为关时它不启用（工作线 F89，tools-more.md §3.2.1），
//! 这一步的工具里就没有它，延迟工具清单、WaitForMcpServers 的去留与主循环都随之是“没有 ToolSearch”。
//!
//! [`ToolCatalog`] 是主线程注册表的弱引用，只给 `deferred_tools_delta` 清单（tools-more.md §3.4）：清单是上下文的来源，上下文
//! 先于注册表建好（注册表里的 Agent 工具要用上下文），注册表跨 tools、ext、agents 三个 crate 组装，只能先建后接，保留最小
//! 形式：绑定一次。缺陷修复（审查 R1-13）：原来没绑定或注册表已释放时静默当作没有工具，现在返回错误。

use std::sync::{Arc, OnceLock, Weak};

use rust_cli_base::late::Late;

use async_trait::async_trait;
use regex::Regex;
use rust_cli_kernel_api::classifier::ClassifierInput;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{ContextRequest, OrchestrationError, ToolRegistry};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use rust_cli_kernel_api::tool_names::TOOL_SEARCH;
use rust_cli_kernel_api::tool_search::{compare_names, is_deferred};
use rust_cli_prompts::models::ToolSearchSetting;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::texts_more as texts;
use crate::tool_search_keywords::{self as keywords, Candidate};
use crate::tool_search_mcp;
use rust_cli_kernel_api::tool::text;
use rust_cli_prompts::Definition;

/// 没有给 `max_results` 时最多返回几个。
const DEFAULT_MAX_RESULTS: usize = 5;

/// 主线程注册表的弱引用（避免 注册表 → 工具 → 上下文 → 清单 → 注册表 的 `Arc` 环；组装的晚绑定，base 的 `late::Late`）。
#[derive(Default)]
pub struct ToolCatalog {
    registry: Late<Weak<dyn ToolRegistry>>,
}

impl ToolCatalog {
    /// 建好主线程注册表后绑定一次；再绑定是组装的缺陷，留着第一个、记 error 日志（工作线 F131：以前静默忽略）。
    pub fn bind(&self, registry: &Arc<dyn ToolRegistry>) {
        if self.registry.bind(Arc::downgrade(registry)).is_err() {
            tracing::error!("the tool catalog was bound twice; keeping the first registry");
        }
    }

    fn main(&self) -> Result<Arc<dyn ToolRegistry>, String> {
        let bound = self
            .registry
            .get()
            .ok_or_else(|| "the tool catalog is not bound to the main tool registry".to_owned())?;
        bound
            .upgrade()
            .ok_or_else(|| "the main tool registry has been released".to_owned())
    }

    /// 这些名字里延迟的工具名，按 UTF-16 码元序（`deferred_tools_delta` 的清单顺序）。
    fn deferred_names(&self, names: &[String], root: &SessionId) -> Result<Vec<String>, String> {
        // 这一步的工具名出自顶层会话的视图（会话级的 MCP 工具只在视图里，工作线 F145）。
        let registry = self.main()?.view(root);
        let mut deferred: Vec<String> = names
            .iter()
            .filter(|name| {
                registry
                    .find(name)
                    .is_some_and(|tool| tool.is_enabled() && is_deferred(tool.as_ref()))
            })
            .cloned()
            .collect();
        // 缺陷修复（R4 F1）：原来按不分大小写的 `compare_names`（近似工具池的 `localeCompare`），
        // MCP 工具夹在内置工具中间。清单按 JS 缺省排序（UTF-16 码元序）：大写开头的内置工具在小写开头的
        // MCP 工具之前。工具名是 ASCII，按字节比较与码元序相同。agent 清单仍按 `localeCompare`（context `listings.rs`）。
        deferred.sort();
        deferred.dedup();
        Ok(deferred)
    }
}

/// `deferred_tools_delta` 的清单来源：这一步的工具里有 ToolSearch 时（乐观判断，tools-more.md §3.2.1），工具集里的延迟
/// 工具，每行是工具名。修改（工作线 F89）：不再看模型；`auto` 而这一步没到门槛时照样宣布。
#[async_trait]
impl ListingSource for ToolCatalog {
    async fn listings(&self, request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        if !request.tool_names.iter().any(|name| name == TOOL_SEARCH) {
            return Ok(Listings::default());
        }
        let deferred_tools = self
            .deferred_names(&request.tool_names, request.root())
            .map_err(|message| OrchestrationError::Failed { message })?
            .into_iter()
            .map(|name| ListingEntry {
                line: name.clone(),
                name,
            })
            .collect();
        Ok(Listings {
            deferred_tools,
            ..Listings::default()
        })
    }
}

/// 这次运行的工具里启用的，按名字排序（ToolSearch 的候选）。
fn enabled(tools: &dyn ToolRegistry) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<_> = tools
        .tools()
        .into_iter()
        .filter(|tool| tool.is_enabled())
        .collect();
    tools.sort_by(|left, right| compare_names(left.name(), right.name()));
    tools
}

pub struct ToolSearchTool {
    definition: Definition,
    /// 入口读到的 `ENABLE_TOOL_SEARCH`（runtime 组装时给出）。
    setting: ToolSearchSetting,
}

impl ToolSearchTool {
    pub fn new(setting: ToolSearchSetting) -> Self {
        Self {
            definition: rust_cli_prompts::definition(TOOL_SEARCH).clone(),
            setting,
        }
    }

    fn candidates(tools: &[Arc<dyn Tool>], all: &[Arc<dyn Tool>]) -> Vec<Candidate> {
        let names: Vec<String> = all.iter().map(|tool| tool.name().to_owned()).collect();
        let prompt = PromptContext {
            tool_names: names,
            mode: PermissionMode::Default,
        };
        tools
            .iter()
            .map(|tool| Candidate {
                name: tool.name().to_owned(),
                description: tool.description(&prompt),
                hint: tool.search_hint().map(str::to_owned),
                mcp: rust_cli_kernel_api::tool_search::is_mcp(tool.as_ref()),
            })
            .collect()
    }

    /// `select:A,B`：先在延迟工具里找，再在全部工具里找（已加载的工具，视为无害的空操作），再按 MCP
    /// 工具名找。返回找到的工具名与找不到的名字（按出现的先后）。
    fn select(
        list: &str,
        deferred: &[Arc<dyn Tool>],
        all: &[Arc<dyn Tool>],
    ) -> (Vec<String>, Vec<String>) {
        let mut found: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let by_name = |tools: &[Arc<dyn Tool>], name: &str| {
            tools
                .iter()
                .find(|tool| rust_cli_kernel_api::tool::answers_to(tool.as_ref(), name))
                .map(|tool| vec![tool.name().to_owned()])
        };
        for name in list
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let matched = by_name(deferred, name)
                .or_else(|| by_name(all, name))
                .unwrap_or_else(|| {
                    all.iter()
                        .filter(|tool| keywords::mcp_tool_name(tool.name()) == Some(name))
                        .map(|tool| tool.name().to_owned())
                        .collect()
                });
            if matched.is_empty() {
                missing.push(name.to_owned());
            }
            for name in matched {
                if !found.contains(&name) {
                    found.push(name);
                }
            }
        }
        (found, missing)
    }

    /// 在这些工具里搜（`select:` 时只看 `list` 里的名字，否则按关键词）。
    fn search(
        query: &str,
        select: Option<&str>,
        max_results: usize,
        all: &[Arc<dyn Tool>],
    ) -> Vec<String> {
        let deferred: Vec<Arc<dyn Tool>> = all
            .iter()
            .filter(|tool| is_deferred(tool.as_ref()))
            .cloned()
            .collect();
        match select {
            Some(list) => Self::select(list, &deferred, all).0,
            None => keywords::search(
                query,
                &Self::candidates(&deferred, all),
                &all.iter()
                    .map(|tool| tool.name().to_owned())
                    .collect::<Vec<_>>(),
                max_results,
            ),
        }
    }
}

fn select_query(query: &str) -> Option<&str> {
    static SELECT: OnceLock<Option<Regex>> = OnceLock::new();
    SELECT
        .get_or_init(|| Regex::new(r"(?is)^select:(.+)$").ok())
        .as_ref()?
        .captures(query)
        .and_then(|captures| captures.get(1))
        .map(|list| list.as_str())
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn classifier_input(&self, _input: &Value) -> ClassifierInput {
        // 没有要判的内容（同对照实现的缺省，auto-classifier.md §15.3）
        ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        TOOL_SEARCH
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    /// `ENABLE_TOOL_SEARCH` 为关时不启用（工作线 F89，tools-more.md §3.2.1）。
    fn is_enabled(&self) -> bool {
        self.setting.allows()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(rust_cli_kernel_api::tool::DEFAULT_MAX_RESULT_CHARS)
    }

    /// 没有专属的权限检查，默认放行。
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let max_results = input
            .get("max_results")
            .and_then(Value::as_f64)
            .filter(|max| max.is_finite() && *max >= 0.0)
            .map_or(DEFAULT_MAX_RESULTS, |max| max as usize);
        // 搜这次运行的工具（tools-more.md §3.5）：每轮的禁用名单与子代理的 agent 定义都已经在里面。
        // 修 bug（重做线 SUBAGENT-SEARCH）：以前搜主线程注册表的视图，子代理里搜得到 agent 定义不给的延迟工具（前台 Explore
        // 选到 NotebookEdit），引用在组装请求时全部去掉、调用被拒。ToolSearch 应按调用它的那次运行的工具搜（出处见 tools-more.md D4）。
        let tools = || enabled(context.tools.as_ref());
        let all = tools();
        let select = select_query(query);
        // 先照常搜；`select:` 时记下找不到的名字。
        let (mut matches, missing) = match select {
            Some(list) => {
                let deferred: Vec<Arc<dyn Tool>> = all
                    .iter()
                    .filter(|tool| is_deferred(tool.as_ref()))
                    .cloned()
                    .collect();
                Self::select(list, &deferred, &all)
            }
            None => (Self::search(query, None, max_results, &all), Vec::new()),
        };
        // 搜不到时看连接中的 MCP 服务器：等它们、再搜一次（tools-more.md §3.5.1，修复线 F36）。
        let mcp = context.tools.mcp();
        let retry = if select.is_some() {
            !missing.is_empty()
        } else {
            matches.is_empty()
        };
        if retry && let Some(mcp) = &mcp {
            // `select:` 只重找找不到的那些名字；点名的文本也是它们。
            let missing_list = missing.join(",");
            let (text_for, list) = match select {
                Some(_) => (missing.join(" "), Some(missing_list.as_str())),
                None => (query.to_owned(), None),
            };
            let search = |tools: &[Arc<dyn Tool>]| Self::search(query, list, max_results, tools);
            let found =
                tool_search_mcp::retry(mcp.as_ref(), &all, &text_for, tools, search, &context)
                    .await?;
            for name in found {
                if !matches.contains(&name) {
                    matches.push(name);
                }
            }
        }
        if matches.is_empty() {
            let note = tool_search_mcp::pending_note(mcp.as_deref());
            return Ok(text(format!("{}{note}", texts::NO_DEFERRED_MATCH)));
        }
        Ok(ToolOutput {
            content: matches
                .into_iter()
                .map(|tool_name| ToolResultContent::ToolReference { tool_name })
                .collect(),
            is_error: false,
            display: Value::Null,
        })
    }
}

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tests;
