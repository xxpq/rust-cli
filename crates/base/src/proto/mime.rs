//! MIME 类型。这里列出的写法只定义在这里（工作线 F122，README §4.1 的 `SINGLE_DEFINITIONS`）。

/// `application/json`。
pub const JSON: &str = "application/json";
/// `text/event-stream`（SSE）。
pub const EVENT_STREAM: &str = "text/event-stream";
pub const TEXT_PLAIN: &str = "text/plain";
pub const TEXT_HTML: &str = "text/html";
pub const TEXT_MARKDOWN: &str = "text/markdown";
pub const PNG: &str = "image/png";
pub const JPEG: &str = "image/jpeg";
pub const GIF: &str = "image/gif";
pub const WEBP: &str = "image/webp";
/// BMP：Node 的图片处理能解，用户附上的 BMP 在预算内原样发（docs/specs/rust-cli/session-lifecycle.md §10.41）。
pub const BMP: &str = "image/bmp";
pub const PDF: &str = "application/pdf";
/// 主类型的前缀：附件按它分图片与视频（session-lifecycle.md §10.41）。
pub const IMAGE_PREFIX: &str = "image/";
pub const VIDEO_PREFIX: &str = "video/";
