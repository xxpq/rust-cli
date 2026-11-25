//! 前端层给模型的产品内容（docs/specs/rust-cli/context.md §15.5）：只有类型，没有前端的文字与能力的名字。
//!
//! 内核只定槽位、放置顺序、包装、角色与增量所在的位置；内容（文字、按前端能力选段、一条输入带的材料怎样写给模型）由入口为
//! 每套内核装的前端层给。没有前端层的内核（测试、嵌入）不发这几种内容。

use serde_json::Value;

use crate::env::WorkspaceRef;
use crate::ids::SessionId;
use crate::input_extras::{PageState, Quote};
use crate::listing::PluginCapabilities;

/// 前端上下文段的 attachment 来源（槽位 A3）：主线程的当步附件，system 角色；增量按最近一次压缩边界之后最后一条的 `data` 算。
pub const CONTEXT_SOURCE: &str = "frontend_context";
/// 输入里引用的插件的提醒的来源（槽位 A5）：user 角色（[`crate::message::AttachmentMessage::role`]）。
pub const PLUGIN_REFERENCE_SOURCE: &str = "plugin_reference";

/// 前端层的模型可见内容（context.md §15.5 的端口表）。
pub trait FrontendPrompt: Send + Sync {
    /// 槽位 A3：这一步的前端上下文段 `(text, data)`，text 未包 `<system-reminder>`；不用发时为 None。`announced` 是最近一次
    /// 压缩边界之后最后一条前端上下文段的 `data`（没有时为 None）。
    fn context(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
        announced: Option<&Value>,
    ) -> Option<(String, Value)>;

    /// 槽位 A5：人类键入的输入里引用的插件 id，按首次出现的顺序。为空时内核不查插件目录。
    fn plugin_references(&self, input: &str) -> Vec<String>;

    /// 槽位 A5：引用的插件这一刻的能力（内核按 id 查的，按引用的顺序）→ 提醒的 `(text, data)`，text 未包装；都跳过时为 None。
    fn plugin_reference(&self, found: Vec<PluginCapabilities>) -> Option<(String, Value)>;

    /// 槽位 A5：一条输入带的引用给模型的文字（内核不包装）。
    fn quotes(&self, quotes: &[Quote]) -> String;

    /// 槽位 A5：一条输入带的页面状态给模型的文字（内核包 `<system-reminder>`）。
    fn page_states(&self, pages: &[PageState]) -> String;
}
