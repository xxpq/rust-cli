//! data URL `data:<类型>;base64,<数据>`：上传的附件在会话 Artifact 里的内容（Node `writePromptAttachment` 的形状，
//! docs/specs/rust-cli/node-store.md §4.4.1）。前端的上传服务写它，主循环物化请求里的 Artifact 图片时读它
//! （session-lifecycle.md §10.40）。

const SCHEME: &str = "data:";
const BASE64_MARK: &str = ";base64";
const SEPARATOR: char = ',';

/// 一个 base64 的 data URL 的两部分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataUrl<'a> {
    pub media_type: &'a str,
    pub data: &'a str,
}

/// 写一个 base64 的 data URL（`data` 已是 base64）。
pub fn base64(media_type: &str, data: &str) -> String {
    format!("{SCHEME}{media_type}{BASE64_MARK}{SEPARATOR}{data}")
}

/// 读一个 base64 的 data URL：只认 base64 的写法，数据不能为空（Node `isUsableDataUrl`）。
pub fn parse(text: &str) -> Option<DataUrl<'_>> {
    let (header, data) = text.strip_prefix(SCHEME)?.split_once(SEPARATOR)?;
    let media_type = header.strip_suffix(BASE64_MARK)?;
    (!data.is_empty()).then_some(DataUrl { media_type, data })
}

#[cfg(test)]
#[path = "data_url_tests.rs"]
mod tests;
