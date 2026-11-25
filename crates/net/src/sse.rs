// 复制自 apps/zcode-cli-rust/crates/model/src/sse.rs（feat/rs 79b6691c55），补上 `event:` 字段。
//! SSE 解码（WHATWG `text/event-stream`）。
//!
//! 输入是任意切分的字节块，输出是完整的事件。每个输入字节只扫描、复制一次；行与事件不限长
//! （一次写 300 KB 文件的工具调用参数就在一条 `data:` 行里）。
//!
//! 事件只带 `event:` 与 `data:` 两个字段；`:` 开头的注释行被忽略。空行结束一个事件；没有 `data:` 行的事件不分发。
//! 流在事件中途结束时，未完成的事件被丢弃（与规范一致）。
//!
//! `id:` 与 `retry:` 属于事件源、不属于某个事件（WHATWG 的 last event ID 与 reconnection time）：解码器记下它们，
//! 续读的一方（MCP streamable HTTP 的 SEP-1699，docs/specs/rust-cli/ext.md §3.10.6）读 [`SseDecoder::last_event_id`]
//! 与 [`SseDecoder::retry`]，换连接时 [`SseDecoder::restart`]。模型的流不用它们。

use std::time::Duration;

/// 一个完整的 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` 字段；没有时为空（规范里的默认类型 `message`）。
    pub event: Option<String>,
    /// 多条 `data:` 行用 `\n` 连接。
    pub data: String,
}

/// SSE 字节流不是合法的 UTF-8。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("SSE stream is not valid UTF-8")]
pub struct SseError;

#[derive(Debug, Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
    /// 当前事件的 `id:`：事件结束时才成为 last event ID（事件没读完就断开时不算）。
    id: Option<String>,
    /// 最近结束的事件给的 ID；空串表示没有（`id:` 的值为空时清空）。
    last_event_id: String,
    retry: Option<Duration>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 最近结束的事件给的 ID（WHATWG 的 last event ID）：之后的事件不带 `id:` 时保留，换连接时也保留。
    pub fn last_event_id(&self) -> Option<&str> {
        Some(self.last_event_id.as_str()).filter(|id| !id.is_empty())
    }

    /// 服务器给的重连时间（`retry:`，毫秒）；没给时为 None。
    pub fn retry(&self) -> Option<Duration> {
        self.retry
    }

    /// 换一条连接接着读同一个事件源：上一条连接没读完的行与事件丢掉，last event ID 与重连时间保留。
    pub fn restart(&mut self) {
        self.line.clear();
        self.event = None;
        self.data.clear();
        self.has_data = false;
        self.id = None;
    }

    /// 送入下一块字节，返回这块字节里完成的事件（可能为空，也可能有多个）。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        for fragment in bytes.split_inclusive(|byte| *byte == b'\n') {
            self.line.extend_from_slice(fragment);
            if fragment.last() != Some(&b'\n') {
                continue;
            }
            let line = std::str::from_utf8(&self.line)
                .map_err(|_| SseError)?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                // WHATWG：事件结束时先更新 last event ID，没有 data 的事件（续读的 priming）也算。
                if let Some(id) = self.id.take() {
                    self.last_event_id = id;
                }
                if self.has_data {
                    events.push(SseEvent {
                        event: self.event.take(),
                        data: std::mem::take(&mut self.data),
                    });
                    self.has_data = false;
                }
                self.event = None;
            } else if let Some(data) = field(line, "data") {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(data);
                self.has_data = true;
            } else if let Some(event) = field(line, "event") {
                self.event = Some(event.to_owned());
            } else if let Some(id) = field(line, "id") {
                // WHATWG：含 NUL 的值忽略。
                if !id.contains('\0') {
                    self.id = Some(id.to_owned());
                }
            } else if let Some(retry) = field(line, "retry") {
                // WHATWG：只由 ASCII 数字组成时才是重连时间，读到就生效；其余（含空值）忽略。
                if retry.bytes().all(|byte| byte.is_ascii_digit())
                    && let Ok(millis) = retry.parse()
                {
                    self.retry = Some(Duration::from_millis(millis));
                }
            }
            self.line.clear();
        }
        Ok(events)
    }
}

/// `name:value` 或 `name: value` 的值（冒号后的第一个空格不属于值）。
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let value = line.strip_prefix(name)?.strip_prefix(':')?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
