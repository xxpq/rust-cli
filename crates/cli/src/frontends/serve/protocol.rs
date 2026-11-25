//! serve 一条连接上的消息（docs/specs/rust-cli/headless.md §8，multi-tenant.md §4）。一条 WS 消息是一个 JSON 对象。
//!
//! 客户端发：第一条是 `session_open`（这一轮的全部输入）或 `session_attach`（断线后续上）；`session_open` 之后是一行
//! stream-json 的 user 消息；一轮之中可以发对照实现控制协议形状的打断。serve 发：headless 的 stream-json 各行，每行加
//! `turn_id` 与 `seq`；出错时一个 `session_error`，然后关连接。
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// 错误帧的 `code`。
pub(crate) mod code {
    /// 第一条消息或 user 消息不合约定。
    pub(crate) const BAD_REQUEST: &str = "bad_request";
    /// 续用的会话不在会话库里（这一轮还没开始，没有写出任何事件）。
    pub(crate) const SESSION_NOT_FOUND: &str = "session_not_found";
    /// 新开的会话 ID 已经有人用了。
    pub(crate) const SESSION_EXISTS: &str = "session_exists";
    /// 同一个用户的同一个 `turn_id` 已经在跑。
    pub(crate) const TURN_EXISTS: &str = "turn_exists";
    /// 续上时这个进程里没有这一轮（不在这里跑、已经收掉，或用户对不上）。
    pub(crate) const TURN_NOT_FOUND: &str = "turn_not_found";
    /// 续上时要的那一行已经不在缓冲里。
    pub(crate) const EVENTS_TRUNCATED: &str = "events_truncated";
    /// 组装或运行失败（原因在 `message`）。
    pub(crate) const FAILED: &str = "failed";
}

/// 连接上的第一条消息。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum First {
    SessionOpen(Box<Open>),
    SessionAttach(Attach),
}

/// 这一轮的输入：现在运行壳经命令行参数、环境变量与部署文件传给 `-p` 的那些（serve-host.md §4）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Open {
    pub(crate) user: String,
    pub(crate) turn_id: String,
    pub(crate) session: Target,
    /// 主 env；不给时只在测试开关下用 serve 本机（multi-tenant.md §7）。
    #[serde(default)]
    pub(crate) primary_env: Option<PrimaryEnvSpec>,
    /// 这一轮的执行票（中转的服务票）。
    #[serde(default)]
    pub(crate) exec_token: Option<String>,
    /// 个人 provider 配置（Node 的 `provider_config.json` 的内容）：只在内存里用。
    #[serde(default)]
    pub(crate) provider_config: Option<Value>,
    /// 同 `--model`。
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// 同 `--effort`。
    #[serde(default)]
    pub(crate) effort: Option<String>,
    /// 同 `--permission-mode`。
    #[serde(default)]
    pub(crate) permission_mode: Option<String>,
    /// 同 `--max-turns`。
    #[serde(default)]
    pub(crate) max_turns: Option<u32>,
    /// 同 `--append-system-prompt`。
    #[serde(default)]
    pub(crate) append_system_prompt: Option<String>,
    /// 同 `--disallowedTools`。
    #[serde(default)]
    pub(crate) disallowed_tools: Vec<String>,
    /// 同 `--include-partial-messages`。
    #[serde(default)]
    pub(crate) partial_messages: bool,
}

/// 新开还是续用哪个会话：新开时是 `--session-id` 的值（会话 ID 是 `sess_<它>`），续用时是 `--resume` 的值。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Target {
    New(String),
    Resume(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PrimaryEnvSpec {
    /// 执行中转后面的沙盒（同部署文件的 `primary_env.relay`，票在 `exec_token`）。
    Relay(RelaySpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelaySpec {
    pub(crate) url: String,
    pub(crate) user: String,
    pub(crate) workdir: String,
    #[serde(default)]
    pub(crate) up_url: Option<String>,
}

/// 断线后续上：从 `after_seq` 之后接着收这一轮的输出。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Attach {
    pub(crate) user: String,
    pub(crate) turn_id: String,
    #[serde(default)]
    pub(crate) after_seq: u64,
}

/// 第一条消息；不合约定时给出错误帧的原因。
pub(crate) fn first(text: &str) -> Result<First, String> {
    serde_json::from_str(text).map_err(|error| format!("bad first message: {error}"))
}

/// 一轮之中客户端发来的一条：打断时给出 `request_id`（对照实现控制协议的形状）；别的消息忽略。
pub(crate) fn interrupt_request(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value["type"] != "control_request" || value["request"]["subtype"] != "interrupt" {
        return None;
    }
    Some(value["request_id"].as_str().unwrap_or_default().to_owned())
}

/// 打断的回执。
pub(crate) fn control_success(request_id: &str) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id},
    })
    .to_string()
}

/// 错误帧。
pub(crate) fn error_frame(code: &str, message: &str) -> String {
    json!({"type": "session_error", "code": code, "message": message}).to_string()
}

/// headless 写出的一行加上 `turn_id` 与 `seq`（放在最后，原有字段的顺序不变）。不是 JSON 对象的行原样包成
/// `{"type": "raw", "text": …}`（headless 的 stream-json 只写对象，这一支只防万一）。
pub(crate) fn tag(line: &str, turn_id: &str, seq: u64) -> String {
    let mut object = match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(object)) => object,
        Ok(_) | Err(_) => {
            let mut object = Map::new();
            object.insert("type".to_owned(), json!("raw"));
            object.insert("text".to_owned(), json!(line));
            object
        }
    };
    object.insert("turn_id".to_owned(), json!(turn_id));
    object.insert("seq".to_owned(), json!(seq));
    Value::Object(object).to_string()
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
