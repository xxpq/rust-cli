//! 分帧与方言（docs/specs/rust-cli/app-protocol.md §3.3）：传输选项，不认识任何协议。
//!
//! - [`Framing::Lines`]：一行一个 JSON（按 LF，去掉行尾的 CR）。超过上限的行丢弃、读循环继续（v4 的写法）。
//! - [`Framing::ContentLength`]：`Content-Length: N\r\n` + 别的头（忽略）+ 空行 + N 字节的正文（JSON-RPC 的 LSP 式分帧）。
//!   头坏或 N 超过上限时帧边界已不可信，读以错误结束（链路随之关闭）。
//! - [`Dialect`]：写出的消息带不带 `"jsonrpc": "2.0"`；解析失败时回不回 id 为 null 的错误。

use std::io;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// 消息之间怎么分界。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Framing {
    /// 按 LF 分行。
    #[default]
    Lines,
    /// `Content-Length` 头 + 正文。
    ContentLength,
}

/// 消息的写法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    /// 没有 `jsonrpc` 字段（对端的 schema 是 strict）；解析失败且认不出 id 时不回复。
    #[default]
    Bare,
    /// JSON-RPC 2.0：每条消息带 `"jsonrpc": "2.0"`；解析失败且认不出 id 时回 id 为 null 的错误。
    JsonRpc2,
}

/// JSON-RPC 2.0 的版本字段。
const JSONRPC: &str = "jsonrpc";
const JSONRPC_VERSION: &str = "2.0";

/// `Content-Length` 头名（比较时不分大小写）。
const CONTENT_LENGTH: &str = "content-length";
/// 头与正文之间、头与头之间的分隔。
const CRLF: &str = "\r\n";
/// 一行头的上限（字节）：头只有名字与数字，超过就是坏帧。
const MAX_HEADER_LINE_BYTES: usize = 1024;

impl Dialect {
    /// 一条消息的文字：`fields` 按顺序写进对象，JSON-RPC 2.0 时在最前面加版本。
    pub fn message(self, fields: Vec<(&str, Value)>) -> String {
        let mut object = Map::new();
        if self == Self::JsonRpc2 {
            object.insert(JSONRPC.to_owned(), json!(JSONRPC_VERSION));
        }
        for (key, value) in fields {
            object.insert(key.to_owned(), value);
        }
        Value::Object(object).to_string()
    }

    /// 同 [`Self::message`]，但每个值已经是 JSON 文字：按顺序拼成对象，不经 `Value`（值不再解析、重编）。
    pub fn message_text(self, fields: &[(&str, &str)]) -> String {
        let mut text = String::from("{");
        if self == Self::JsonRpc2 {
            text.push_str(&format!("\"{JSONRPC}\":\"{JSONRPC_VERSION}\""));
        }
        for (key, value) in fields {
            if text.len() > 1 {
                text.push(',');
            }
            text.push_str(&json!(key).to_string());
            text.push(':');
            text.push_str(value);
        }
        text.push('}');
        text
    }

    /// 解析失败、认不出 id 时要不要回 id 为 null 的错误（JSON-RPC 2.0 §5）。
    pub fn answers_unknown_ids(self) -> bool {
        self == Self::JsonRpc2
    }

    /// 正文不是 JSON 时关链路（「读失败即链路断开」，设计稿 01 §3.2）：对端写来的内容
    /// 这一端读不了，按序投递已不成立。v4（`Bare`）照旧只记诊断、接着读。
    pub fn closes_on_unreadable(self) -> bool {
        self == Self::JsonRpc2
    }
}

/// 读到的一条消息：内容，或者超过上限（只在 [`Framing::Lines`]：已丢弃到行尾）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Read {
    Text(String),
    TooLong,
}

/// 帧坏了：读循环以这个错误结束。
fn broken(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// 读一条消息。读到结束时返回 None。
pub(crate) async fn read<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    framing: Framing,
    max: usize,
) -> io::Result<Option<Read>> {
    match framing {
        Framing::Lines => read_line(reader, max).await,
        Framing::ContentLength => read_content_length(reader, max).await,
    }
}

/// 一条消息的字节（写者用）。
pub(crate) fn encode(framing: Framing, text: &str) -> Vec<u8> {
    match framing {
        Framing::Lines => format!("{text}\n").into_bytes(),
        Framing::ContentLength => {
            let mut bytes = format!("Content-Length: {}{CRLF}{CRLF}", text.len()).into_bytes();
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
    }
}

/// 读一行（去掉 LF 与它前面的 CR）。
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Option<Read>> {
    let mut buffer = Vec::new();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buffer.is_empty() && !too_long {
                return Ok(None);
            }
            break;
        }
        let (chunk, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..at], Some(at + 1)),
            None => (available, None),
        };
        if !too_long && buffer.len() + chunk.len() > max {
            too_long = true;
            buffer = Vec::new();
        }
        if !too_long {
            buffer.extend_from_slice(chunk);
        }
        let consumed = done.unwrap_or(available.len());
        reader.consume(consumed);
        if done.is_some() {
            break;
        }
    }
    if too_long {
        return Ok(Some(Read::TooLong));
    }
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
    Ok(Some(Read::Text(
        String::from_utf8_lossy(&buffer).into_owned(),
    )))
}

/// 读一行头（不含 CRLF）。还没有读到任何字节就结束时返回 None；行太长或中途结束是坏帧。
async fn header_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    first: bool,
) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if first && line.is_empty() {
                return Ok(None);
            }
            return Err(broken("the input ended inside a frame header"));
        }
        let (chunk, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..at], Some(at + 1)),
            None => (available, None),
        };
        if line.len() + chunk.len() > MAX_HEADER_LINE_BYTES {
            return Err(broken("a frame header line is too long"));
        }
        line.extend_from_slice(chunk);
        let consumed = done.unwrap_or(available.len());
        reader.consume(consumed);
        if done.is_some() {
            break;
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| broken("a frame header is not UTF-8"))
}

async fn read_content_length<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Option<Read>> {
    let mut length = None;
    let mut first = true;
    loop {
        let Some(line) = header_line(reader, first).await? else {
            return Ok(None);
        };
        first = false;
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(broken(format!("a frame header without a colon: {line:?}")));
        };
        if name.trim().eq_ignore_ascii_case(CONTENT_LENGTH) {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| broken(format!("Content-Length is not a number: {value:?}")))?;
            length = Some(parsed);
        }
    }
    let Some(length) = length else {
        return Err(broken("a frame without Content-Length"));
    };
    if length > max {
        return Err(broken(format!(
            "a frame of {length} bytes exceeds the limit of {max} bytes"
        )));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| broken(format!("the input ended inside a frame body: {error}")))?;
    Ok(Some(Read::Text(
        String::from_utf8_lossy(&body).into_owned(),
    )))
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;
