//! 工具输出（01 §19.5，02 §5，protocol-v5.md §68.0 第 6、7 条；用户定 R4）：行上只带预览，完整的文字走查询 `tool.output`。
//!
//! - 预览：全文的 UTF-8 字节数超过 [`TOOL_OUTPUT_PREVIEW_BYTES`] 时，`output.text` 只带开头的一半，`output.truncated` 带结尾的一半与全文
//!   字节数（头尾分开放：不认识 `truncated` 的读端只看到开头一段，不会把头尾错接成一段）。活会话与冷读用同一个函数，冷热等价不变。
//! - 全文：从会话库读（与冷读同一个视图，不激活会话），按调用 id 找这次调用的结果。结果在 `ToolTimeRecorded` 之后才落库，之前找不到。

use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::StoreQueries;
use serde_json::{Value, json};

use super::lane::cold_view;
use super::queries::{fault, ok};
use super::rows::{self, Part, keys};
use crate::consts::TOOL_OUTPUT_PREVIEW_BYTES;
use crate::errors::Fault;
use crate::generated::contract::codes;
use crate::generated::core_v1::Cursor;
use crate::server::Workspaces;

/// `output.truncated` 与它里面的键（02 §5）。
pub(super) const TRUNCATED: &str = "truncated";
const TOTAL_BYTES: &str = "totalBytes";
const TAIL: &str = "tail";

/// `<= at` 的最后一个字符边界。
fn floor_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// `>= at` 的第一个字符边界。
fn ceil_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// 行上的工具输出（v4 `toolOutputSchema` 的形状 `{text, truncated?}`）与出错时 `error` 的文字：没超预算的原样；超了只带开头，结尾进
/// `truncated.tail`。
pub(super) fn preview(text: &str) -> (Value, &str) {
    if text.len() <= TOOL_OUTPUT_PREVIEW_BYTES {
        return (json!({ keys::TEXT: text }), text);
    }
    let half = TOOL_OUTPUT_PREVIEW_BYTES / 2;
    let head = &text[..floor_boundary(text, half)];
    let tail = &text[ceil_boundary(text, text.len() - half)..];
    let output = json!({
        keys::TEXT: head,
        TRUNCATED: { TOTAL_BYTES: text.len(), TAIL: tail },
    });
    (output, head)
}

/// 视图里这次调用的结果的全文（最后一个同 id 的结果；续写的调用会有补的结果）。
fn stored(view: &SessionView, row: &str) -> Option<String> {
    view.messages
        .iter()
        .rev()
        .flat_map(|message| rows::parts(message).into_iter().rev())
        .find_map(|part| match part {
            Part::Result { id, text, .. } if id == row => Some(text),
            _ => None,
        })
}

/// `tool.output{rowId}` 的答案（在任务里）：冷读这个会话，取这次调用的结果；答案带发起查询那一刻的 cursor。没有（不是工具行、还没落库、
/// 改写历史之后不在了）回 `precondition.notFound`，不回空。
pub(super) async fn read(
    workspaces: Arc<dyn Workspaces>,
    queries: Arc<dyn StoreQueries>,
    key: String,
    sid: String,
    row: String,
    cursor: Cursor,
) -> Result<Value, Fault> {
    let (_, view, _) = cold_view(workspaces, queries, key, SessionId::new(&sid))
        .await
        .map_err(|rejection| fault(rejection.code, rejection.message))?;
    let text = stored(&view, &row).ok_or_else(|| {
        fault(
            codes::PRECONDITION_NOT_FOUND,
            format!("no stored tool result {row} in this session"),
        )
    })?;
    Ok(ok(json!({ keys::TEXT: text }), cursor))
}

#[cfg(test)]
#[path = "tool_output_tests.rs"]
mod tests;
