//! 前端上下文段（`frontend_context`，docs/specs/rust-cli/context.md §15.6；用户 2026-11-12 的 Q2：App 要这一段，照 Node 原文，
//! 放增量附件，按前端能力选段落）。
//!
//! 文字是 Node `core/src/context/sections/desktop.ts` 的原文（`prompts/app/frontend_*.txt`）；这里按这套内核能宣布的节
//! （[`Renders`]，Gen UI 已按插件集定好）选段落、填 Gen UI 的目录。增量：与最近一次压缩边界之后最后宣布的段落不同时发整段；
//! 一节都没有、之前宣布过（例如在终端里续写 App 的会话）时发「不再适用」那一句。槽位、位置与包装在内核（context）。

use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_prompts::reminder::escape;
use rust_cli_prompts::texts::render;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Renders;
use super::texts;

/// 宣布的段落，也是 `data` 的形状（下一步按它算增量）。每一项对应一节，按 Node 的顺序：Interactive views、Files & URLs、
/// Inline Code Comments。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Shown {
    /// Interactive views 一节与这个会话的 Gen UI 目录：前端渲染 Gen UI，且这套内核加载了要它的插件（visualize）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gen_ui: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    file_links: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    code_comments: bool,
}

impl Shown {
    fn is_empty(&self) -> bool {
        self.gen_ui.is_none() && !self.file_links && !self.code_comments
    }
}

/// 这个会话的 Gen UI 输出目录（Node `getGenUiOutputDirectory`，`packages/shared/src/node/genUiPaths.ts`）：根目录下
/// `sha256(JSON.stringify([工作区, 会话 id]))` 的 hex。本机工作区没有远端身份，键取工作区根（Node `buildGenUiScopeKey`）。
fn gen_ui_directory(root: &str, workspace: &WorkspaceRef, session: &SessionId) -> String {
    let key = json!([workspace.root.as_str(), session.as_str()]).to_string();
    let digest = rust_cli_base::hash::sha256_hex(key.as_bytes());
    PathStyle::infer(root).join(root, &[digest.as_str()])
}

/// 这一刻要宣布的段落。
fn shown(renders: &Renders, (workspace, session): (&WorkspaceRef, &SessionId)) -> Shown {
    Shown {
        gen_ui: renders
            .gen_ui
            .as_ref()
            .map(|root| gen_ui_directory(root.as_str(), workspace, session)),
        file_links: renders.file_links,
        code_comments: renders.code_comments,
    }
}

/// 整段：标题与各节空行连接（同 Node 的拼法）；一节都没有时是「不再适用」那一句。目录写在 Gen UI 一节末尾（Node 写在环境段，
/// 本工程的环境段是对照实现的槽位，不放产品层的行，§15.6）。
fn text(shown: &Shown) -> String {
    if shown.is_empty() {
        return texts::frontend_gone().to_owned();
    }
    let mut parts = vec![texts::frontend_heading().to_owned()];
    if let Some(directory) = &shown.gen_ui {
        let line = render(
            texts::frontend_gen_ui_directory(),
            &[("path", &escape(directory))],
        );
        parts.push(format!("{}\n{line}", texts::frontend_gen_ui()));
    }
    if shown.file_links {
        parts.push(texts::frontend_files().to_owned());
    }
    if shown.code_comments {
        parts.push(texts::frontend_code_comments().to_owned());
    }
    parts.join("\n\n")
}

/// 这一步的前端上下文：`(text, data)`，text 未包 `<system-reminder>`；与上次宣布的相同、或从没宣布过且这一刻什么都没有时为
/// None。`announced` 是最近一次压缩边界之后最后一条的 `data`（内核找出来，压缩附件里重发也按它，§15.6）；形状不对的当作没宣布过。
pub(super) fn announce(
    renders: &Renders,
    scope: (&WorkspaceRef, &SessionId),
    announced: Option<&Value>,
) -> Option<(String, Value)> {
    let current = shown(renders, scope);
    let announced: Option<Shown> =
        announced.and_then(|data| serde_json::from_value(data.clone()).ok());
    match announced {
        Some(previous) if previous == current => None,
        None if current.is_empty() => None,
        _ => Some((text(&current), json!(current))),
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
