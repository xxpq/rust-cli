//! 上下文里各类清单的来源（docs/specs/rust-cli/context.md §4）。
//!
//! 每一步把“现在可用的”与“已经告诉过模型的”比较，只发增量 attachment：
//! 延迟工具（`deferred_tools_delta`）、agent 类型（`agent_listing_delta`）、skill（`skill_listing`）。
//! 清单内容由对应能力提供（延迟工具：query / tools；agent：agents；skill：ext），runtime 组装；
//! context 只算增量、渲染文本。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::orchestration::{ContextRequest, OrchestrationError};

/// 清单里的一项。`name` 用来算增量；`line` 是渲染进提示词的整行，
/// 例如 agent 的 `- Explore: … (Tools: …)`、skill 的 `- name: description`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingEntry {
    pub name: String,
    pub line: String,
}

/// 嵌套目录里发现的一个 skill 目录（ext.md §2.6.1）：context 据它发 `dynamic_skill`，一个目录只宣布一次。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicSkillDir {
    /// skill 目录的路径（已宣布的按它认）。
    pub dir: String,
    /// 相对 cwd 的路径（`pkg/.zcode/skills`），写进提醒。
    pub display: String,
    /// 这个目录里的 skill 合并之后的名字，按列目录的原始顺序。
    pub names: Vec<String>,
}

/// 这一步可用的清单。为空表示没有可宣布的内容（对应能力还没接入时也是空）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listings {
    /// 经 ToolSearch 按需加载、只宣布名字的工具。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_tools: Vec<ListingEntry>,
    /// Agent 工具可用的子代理类型。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<ListingEntry>,
    /// Skill 工具可用的 skill。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<ListingEntry>,
    /// 这一步从嵌套目录发现的 skill 目录（`dynamic_skill`，docs/specs/rust-cli/ext.md §2.6.1），按发现的顺序。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamic_skills: Vec<DynamicSkillDir>,
}

#[async_trait]
pub trait ListingSource: Send + Sync {
    async fn listings(&self, request: &ContextRequest) -> Result<Listings, OrchestrationError>;

    /// 带历史的清单：有的清单取决于对话里发生过的事（例如 `paths` 条件 skill 在触及匹配的文件后才出现，
    /// docs/specs/rust-cli/ext.md §2.6）。`history` 是最近一次压缩边界之后的部分。缺省忽略历史。
    async fn listings_for(
        &self,
        request: &ContextRequest,
        _history: &[Message],
    ) -> Result<Listings, OrchestrationError> {
        self.listings(request).await
    }

    /// 已连接的 MCP 服务器给出的 instructions（`mcp_instructions_delta`，ext.md §3.6）：
    /// `name` 是服务器名，`line` 是渲染好的整段（`## <server>\n<instructions>`）。缺省没有。
    async fn mcp_instructions(
        &self,
        _request: &ContextRequest,
    ) -> Result<Vec<ListingEntry>, OrchestrationError> {
        Ok(Vec::new())
    }

    /// 这次会话调用过的 skill（压缩后带回，docs/specs/rust-cli/compact.md §3.4）：`history` 是全部历史（含之前的压缩
    /// 边界之前的部分），最近调用的在前，同名只留最近一次。缺省没有。
    async fn invoked_skills(
        &self,
        _request: &ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<InvokedSkill>, OrchestrationError> {
        Ok(Vec::new())
    }

    /// 人类输入中间写的 `/<名字>` 里对得上可调用 skill 的名字（`skill_mention`，docs/specs/rust-cli/ext.md §2.8、
    /// context.md §4.4），同一个 skill 一次、按出现的顺序。`input` 是这条输入的文字。不是人类键入的输入
    /// （`-p`、stream-json）返回空。缺省没有。
    async fn skill_mentions(
        &self,
        _request: &ContextRequest,
        _input: &str,
    ) -> Result<Vec<String>, OrchestrationError> {
        Ok(Vec::new())
    }

    /// 输入里引用的插件（`plugin_reference`，context.md §15.11；id 由前端层解析）：认识、启用的插件这一刻的能力，按 `ids` 的
    /// 顺序；不认识、停用的不给。缺省没有。
    async fn plugin_references(
        &self,
        _request: &ContextRequest,
        _ids: &[String],
    ) -> Result<Vec<PluginCapabilities>, OrchestrationError> {
        Ok(Vec::new())
    }
}

/// 输入里引用的一个插件这一刻可用的能力（context.md §15.11）：只有标识符，各自排好序。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// `<name>@<marketplace>`。
    pub id: String,
    /// 来自这个插件的 skill（`<插件名>:<名字>`）。
    pub skills: Vec<String>,
    /// 这个插件的 MCP 服务器里连着、这一步有工具的（`plugin:<插件名>:<server>`）。
    pub mcp_servers: Vec<String>,
    /// 来自这个插件的 agent 类型（`<插件名>:…`）。
    pub subagents: Vec<String>,
}

/// 调用过的一个 skill：名字、来源路径（`<来源>:<名字>`）与调用时展开的正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokedSkill {
    pub name: String,
    pub path: String,
    pub content: String,
}
