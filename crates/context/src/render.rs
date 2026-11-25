//! 渲染用的小工具（列表与转义）。

use rust_cli_kernel_api::message::{AttachmentMessage, Message};

/// 一行文本或一组子项（字符串是 ` - x`，数组的每项是 `  - x`）。
pub(crate) enum Bullet {
    Line(String),
    Nested(Vec<String>),
}

pub(crate) fn bullets(items: Vec<Bullet>) -> String {
    let mut lines = Vec::new();
    for item in items {
        match item {
            Bullet::Line(line) => lines.push(format!(" - {line}")),
            Bullet::Nested(children) => {
                lines.extend(children.into_iter().map(|child| format!("  - {child}")));
            }
        }
    }
    lines.join("\n")
}

/// 最近一次压缩边界之后的历史（规则只在 kernel-api 的 `history`，ext 的条件 skill 也用它）。
pub(crate) use rust_cli_kernel_api::history::since_boundary;

/// 历史里某个来源的 attachment，按出现顺序。
pub(crate) fn attachments<'a>(
    history: &'a [Message],
    source: &'a str,
) -> impl DoubleEndedIterator<Item = &'a AttachmentMessage> {
    history.iter().filter_map(move |message| match message {
        Message::Attachment(attachment) if attachment.source == source => Some(attachment),
        _ => None,
    })
}
