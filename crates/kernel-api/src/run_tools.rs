//! 一次运行看到的工具（docs/specs/rust-cli/tools-more.md §3.5、session-lifecycle.md §10.15，重做线 SUBAGENT-SEARCH）。
//!
//! 注册表（主线程的，或子代理按 agent 定义过滤的那一份）在顶层会话的视图，去掉这次运行不许用的工具（每轮的禁用名单）。
//! 主循环在运行开始时建一次：这一步的清单、快照与查找都在它上面，模型仍调用名单里的工具时找不到它；执行器把同一个对象放进
//! 每个调用的上下文（[`crate::tool::ToolUseContext::tools`]），ToolSearch 从它搜，Agent 工具从它取派生的子代理继承的名单。
//!
//! 名单只在这里。以前同一个事实有两条路：调用上下文的 `disallowed_tools`（给 ToolSearch）与 agents 的 RunBook（给 Agent
//! 工具）；ToolSearch 另搜主线程注册表的视图，子代理里搜得到 agent 定义不给的工具。

use std::sync::Arc;

use crate::ids::SessionId;
use crate::mcp_state::McpConnections;
use crate::orchestration::{ToolRegistry, ToolSnapshot};
use crate::tool::Tool;

pub struct RunTools {
    inner: Arc<dyn ToolRegistry>,
    disallowed: Vec<String>,
}

impl RunTools {
    /// `registry`（已经是这次运行的视图）去掉 `disallowed` 里的工具（工具名，与模型看到的名字相同）。
    pub fn new(registry: Arc<dyn ToolRegistry>, disallowed: &[String]) -> Self {
        Self {
            inner: registry,
            disallowed: disallowed.to_vec(),
        }
    }

    /// 这次运行不许用的工具：这次运行派生的子代理继承它（session-lifecycle.md §10.15）。
    pub fn disallowed(&self) -> &[String] {
        &self.disallowed
    }

    fn allowed(&self, tool: &Arc<dyn Tool>) -> bool {
        !self.disallowed.iter().any(|name| name == tool.name())
    }
}

impl ToolRegistry for RunTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.inner
            .tools()
            .into_iter()
            .filter(|tool| self.allowed(tool))
            .collect()
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner.find(name).filter(|tool| self.allowed(tool))
    }

    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner.withheld(name).filter(|tool| self.allowed(tool))
    }

    fn hides(&self, name: &str) -> bool {
        self.disallowed.iter().any(|hidden| hidden == name) || self.inner.hides(name)
    }

    fn keeps_order(&self) -> bool {
        self.inner.keeps_order()
    }

    fn snapshot(&self, session: &SessionId) -> ToolSnapshot {
        let ToolSnapshot { tools, mcp } = self.inner.snapshot(session);
        ToolSnapshot {
            tools: tools
                .into_iter()
                .filter(|tool| self.allowed(tool))
                .collect(),
            mcp,
        }
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        self.inner.mcp()
    }

    fn view(self: Arc<Self>, root: &SessionId) -> Arc<dyn ToolRegistry> {
        Arc::new(Self {
            inner: self.inner.clone().view(root),
            disallowed: self.disallowed.clone(),
        })
    }
}

#[cfg(test)]
#[path = "run_tools_tests.rs"]
mod tests;
