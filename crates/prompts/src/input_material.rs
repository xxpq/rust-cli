//! 附件给模型的写法（docs/specs/rust-cli/context.md §15.8，槽位 A5）：上传的文字与附件占位。
//!
//! 以前写在 kernel-api 的 `input_extras` 里（重做线 CTX-P05 的 F5 搬来）：kernel-api 只留类型与来源名，给模型的文字在
//! `prompts/context/input_*.txt`。调用方是 query 与 tools 的附件读取，attachment 的来源与角色仍由 kernel-api 定（user 角色）。
//! 引用与页面状态的写法在 App 前端层（§15.5）。文字逐字照 Node（出处见各函数）。

use crate::texts::{self, render};

/// 标签的长度上限（字符）与截断后的结尾（Node `sanitizeAttachmentLabel`）。
const LABEL_MAX_CHARS: usize = 200;
const LABEL_ELLIPSIS: &str = "...";
/// 正文里的 `<system-reminder` / `</system-reminder`（不分大小写，后面不接名字字符）：开头的 `<` 写成 `&lt;`，上传的文字
/// 不能闭合包装（Node `escapeNestedSystemReminderTags` 的 `/<\/?system-reminder\b/gi`）。
const REMINDER_TAG_NAME: &str = "system-reminder";
const ESCAPED_LESS_THAN: &str = "&lt;";
/// Node 的附件占位（§10.41，`attachment-placeholder.ts` 的 `[Attached <mime>: <placeholder>]`）：读不出、解不开、模型不收的附件。
pub fn placeholder(media_type: &str, label: &str) -> String {
    render(
        texts::input_placeholder(),
        &[("media_type", media_type), ("label", label)],
    )
}

/// 上传的文字（Node `buildInlineAttachmentReminderBodies`，`core/src/system-reminder/prompt-attachment.ts`）：`<system-reminder>`
/// 包装，正文里提醒的标签转义。
pub fn upload_text(name: &str, text: &str) -> String {
    let first = match attachment_label(name) {
        Some(label) => render(texts::input_upload_named(), &[("label", &label)]),
        None => texts::input_upload_unnamed().to_owned(),
    };
    let body = render(
        texts::input_upload_text(),
        &[("first", &first), ("text", text)],
    );
    crate::reminder::wrap(&escape_reminder_tags(&body))
}

/// 标签：空白压成一个空格、去掉首尾；空的没有标签；超过 200 个字符截成 197 个加 `...`（Node `sanitizeAttachmentLabel`）。
fn attachment_label(name: &str) -> Option<String> {
    let label = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        return None;
    }
    if label.chars().count() <= LABEL_MAX_CHARS {
        return Some(label);
    }
    let kept: String = label
        .chars()
        .take(LABEL_MAX_CHARS - LABEL_ELLIPSIS.len())
        .collect();
    Some(format!("{kept}{LABEL_ELLIPSIS}"))
}

/// `<` 之后是不是一个提醒的标签名：可选的 `/`、不分大小写的 `system-reminder`、之后不是 ASCII 的名字字符（JS 的 `\b`）。
fn opens_reminder_tag(rest: &str) -> bool {
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let Some(name) = rest.get(..REMINDER_TAG_NAME.len()) else {
        return false;
    };
    name.eq_ignore_ascii_case(REMINDER_TAG_NAME)
        && !rest[REMINDER_TAG_NAME.len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_')
}

/// 正文里提醒的开闭标签：开头的 `<` 写成 `&lt;`。
fn escape_reminder_tags(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for (index, ch) in body.char_indices() {
        if ch == '<' && opens_reminder_tag(&body[index + 1..]) {
            out.push_str(ESCAPED_LESS_THAN);
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
#[path = "input_material_tests.rs"]
mod tests;
