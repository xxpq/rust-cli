//! 一条输入带的引用与页面状态给模型的写法（docs/specs/rust-cli/context.md §15.8，槽位 A5）。会话 actor 开回合时经端口取，
//! 位置、包装与来源（user 角色）在内核。CTX-P05 时在 prompts 的 `input_material`，APP-CTX 搬来；文字逐字照 Node（出处见各函数）。

use rust_cli_kernel_api::input_extras::{PageState, Quote};
use rust_cli_prompts::texts::render;
use serde::Serialize;

use super::texts;

/// JSON 里的 `<` 写成转义（同 Node 的 `replaceAll("<", "\\u003c")`）：页面的状态不能闭合标签。
const LESS_THAN: &str = "<";
const LESS_THAN_ESCAPED: &str = "\\u003c";

/// 引用给模型的 JSON（Node `withConversationQuotes`；键序与 v4 `conversationQuoteSchema` 相同，`sentAt` 是 ISO 时刻）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuoteForModel<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sent_at: Option<String>,
}

/// 引用（Node `withConversationQuotes`，`core/src/runtime/helpers/conversation-quotes.ts`）：不加 `<system-reminder>`。
pub(super) fn quotes(quotes: &[Quote]) -> String {
    let shown: Vec<QuoteForModel<'_>> = quotes
        .iter()
        .map(|quote| QuoteForModel {
            text: &quote.text,
            path: quote.path.as_deref(),
            sender_name: quote.sender_name.as_deref(),
            sender_id: quote.sender_id.as_deref(),
            message_id: quote.message_id.as_deref(),
            sent_at: quote.sent_at_ms.map(rust_cli_base::js::date::iso_timestamp),
        })
        .collect();
    let json = serde_json::json!(shown).to_string();
    render(texts::input_quote(), &[("json", &json)])
}

/// 页面状态（Node `attachGenUiContext`，`core/src/agent/message-history.ts`）：不可信标签，JSON 里的 `<` 转义；`<system-reminder>`
/// 由内核包。
pub(super) fn page_states(pages: &[PageState]) -> String {
    let json = serde_json::json!(pages)
        .to_string()
        .replace(LESS_THAN, LESS_THAN_ESCAPED);
    render(texts::input_page_state(), &[("json", &json)])
}

#[cfg(test)]
#[path = "material_tests.rs"]
mod tests;
