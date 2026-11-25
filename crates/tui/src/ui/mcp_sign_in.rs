//! 远程 MCP 服务器的 Sign in 在 transcript 里的说法（docs/specs/rust-cli/tui-manage.md §6.5）：开始时的说明与地址、完成与失败。
//! 这里没有状态：授权的进度在内核（`Kernel::mcp_sign_in` 的 future），结果来自它给的服务器状态。
use rust_cli_kernel_api::manage::{McpServerInfo, McpServerState};

use super::i18n::Lang;
use super::mcp_control::state_word;

/// 浏览器打开没有。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Opened,
    /// 远程 shell 或打不开：请用户自己打开。
    Yourself,
}

/// 开始时的三行：在做什么、浏览器、地址（整段，终端里可以选中复制）。
pub fn signing_in(name: &str, url: &str, browser: Browser, lang: Lang) -> String {
    let (head, how) = match (lang, browser) {
        (Lang::En, Browser::Opened) => (
            format!("Signing in to {name}…"),
            "Opened the sign-in page in your browser. Sign in there.",
        ),
        (Lang::En, Browser::Yourself) => (
            format!("Signing in to {name}…"),
            "Open this link on any device and sign in there.",
        ),
        (Lang::Zh, Browser::Opened) => (
            format!("正在登录 {name}…"),
            "已在浏览器里打开登录页，请在那里登录。",
        ),
        (Lang::Zh, Browser::Yourself) => (
            format!("正在登录 {name}…"),
            "在任意设备上打开这个链接并登录。",
        ),
    };
    format!("{head}\n{how}\n{url}")
}

/// 授权完成、重连之后。
pub fn signed_in(name: &str, info: &McpServerInfo, lang: Lang) -> String {
    let tools = info.tools;
    match (lang, info.state) {
        (Lang::En, McpServerState::Connected) => format!(
            "Signed in to {name}. Connected · {tools} {}.",
            if tools == 1 { "tool" } else { "tools" }
        ),
        (Lang::Zh, McpServerState::Connected) => {
            format!("已登录 {name}，已连接 · {tools} 个工具。")
        }
        (Lang::En, state) => format!(
            "Signed in to {name}, but it isn't connected ({}).",
            state_word(state, lang)
        ),
        (Lang::Zh, state) => format!("已登录 {name}，但还没连上（{}）。", state_word(state, lang)),
    }
}

pub fn sign_in_failed(name: &str, reason: &str, lang: Lang) -> String {
    let reason = reason.trim_end_matches('.');
    match lang {
        Lang::En => format!("Couldn't sign in to {name}: {reason}."),
        Lang::Zh => format!("没能登录 {name}：{reason}。"),
    }
}

/// 又按了一次 Sign in：前一次丢下了。
pub fn sign_in_cancelled(name: &str, lang: Lang) -> String {
    match lang {
        Lang::En => format!("Cancelled signing in to {name}."),
        Lang::Zh => format!("已取消登录 {name}。"),
    }
}

#[cfg(test)]
#[path = "mcp_sign_in_tests.rs"]
mod tests;
