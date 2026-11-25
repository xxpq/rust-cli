//! 设置页里 kit 自己写的字（docs/specs/rust-cli/tui-kit.md §4.5「语言」，2026-11-12）：英文缺省是 [`SETTINGS_WORDS`]（与以前
//! 写死的字一字不差），壳按界面语言传译文，同 transcript 的条目（`notices::NoticeWords` 一类）。铺法、墨色与放不下时怎样缩
//! 不随字变。
use super::hints::Asking;
use super::{Applies, McpStatus, SECTIONS, Sec};

/// kit 写的字。
#[derive(Clone, Copy)]
pub struct SettingsWords {
    /// 段：与 [`SECTIONS`] 同序（id 与组相同，名字与说明换成这种语言）。
    pub sections: &'static [Sec; SECTIONS.len()],
    /// 头一行的 `Settings`。
    pub title: &'static str,
    /// 搜索时头一行的命中数：`3 matches`。
    pub matches: fn(usize) -> String,
    /// 开关的值。
    pub on: &'static str,
    pub off: &'static str,
    /// 选项的值是缺省时后面的词（前面空两格）。
    pub default: &'static str,
    /// 没有选模型。
    pub not_set: &'static str,
    /// 选项列表里现在的值后面的词。
    pub current: &'static str,
    /// 确认里不做的那一项。
    pub keep_it: &'static str,
    /// 在浏览器里登录时选中行下面的两句。
    pub signing_in: &'static str,
    pub waiting: &'static str,
    /// 测连接。
    pub testing: &'static str,
    pub connected: &'static str,
    /// ` · answered in 0.4 s`（秒）。
    pub answered: fn(f64) -> String,
    /// 文件行下面：`⏎ opens it in <编辑器>.`；编辑器是 "" 时是你的编辑器。
    pub opens_in: fn(&str) -> String,
    /// 窗口截掉的边上：`↑ 3 more`、`↓ 2 more`。
    pub above: fn(usize) -> String,
    pub below: fn(usize) -> String,
    /// 选中行下面生效的时刻，与保存之后输入框上方说的。
    pub applies: fn(Applies) -> &'static str,
    pub saved: fn(Applies) -> &'static str,
    /// 请求开着时输入框上方的一句（`esc` 前面）与 `esc` 后面的话。
    pub asking: fn(Asking) -> &'static str,
    pub goes_back: &'static str,
    /// 状态栏的键的说明：英文词换成这种语言的；不认识的（壳传进来的、已经译好的）原样。
    pub key: fn(&str) -> Option<&'static str>,
    /// `⏎ edit in <编辑器>`。
    pub edit_in: fn(&str) -> String,
    /// 输入框的占位：搜索、键入的值（不是文字的行、密钥、缺省）。
    pub search: &'static str,
    pub type_text: &'static str,
    pub type_key: &'static str,
    pub type_value: &'static str,
    /// 键入密钥时值的名字后面的话（前面空两格）。
    pub hidden: &'static str,
    /// MCP 服务器的状态词，与连上时的工具数（`3 tools`）。
    pub mcp: fn(McpStatus) -> &'static str,
    pub tools: fn(usize) -> String,
}

/// 英文（Puffin 的字）。
pub const SETTINGS_WORDS: SettingsWords = SettingsWords {
    sections: &SECTIONS,
    title: "Settings",
    matches: |n| format!("{n} {}", if n == 1 { "match" } else { "matches" }),
    on: "on",
    off: "off",
    default: "default",
    not_set: "not set",
    current: "current",
    keep_it: "Keep it",
    signing_in: "Opened the sign-in page in your browser. Sign in there.",
    waiting: "Waiting for you to sign in",
    testing: "Testing…",
    connected: "Connected",
    answered: |s| format!(" · answered in {s:.1} s"),
    opens_in: |editor| {
        if editor.is_empty() {
            "⏎ opens it in your editor.".to_owned()
        } else {
            format!("⏎ opens it in {editor}.")
        }
    },
    above: |n| format!("↑ {n} more"),
    below: |n| format!("↓ {n} more"),
    applies: Applies::short,
    saved: Applies::saved,
    asking: |a| match a {
        Asking::Question => "The agent has a question.",
        Asking::Plan => "The agent has a plan for you.",
        Asking::Permission => "The agent is asking for permission.",
    },
    goes_back: " goes back to answer it.",
    key: |_| None,
    edit_in: |editor| format!("edit in {editor}"),
    search: "Search settings",
    type_text: "Type the text",
    type_key: "Type or paste the key",
    type_value: "Type a value",
    hidden: "hidden as you type",
    mcp: |s| match s {
        McpStatus::Off => "off",
        McpStatus::NeedsSignIn => "needs sign-in",
        McpStatus::Connected { .. } => "connected",
        McpStatus::Failed => "failed",
        McpStatus::Untrusted => "needs review",
        McpStatus::Disconnected => "disconnected",
        McpStatus::Connecting => "connecting",
    },
    tools: |n| super::plural(n, "tool"),
};

impl SettingsWords {
    /// 键的说明（[`Self::key`]，不认识的原样）。
    pub fn key_word<'a>(&self, word: &'a str) -> &'a str {
        (self.key)(word).unwrap_or(word)
    }

    /// 段 `id` 的这种语言的名字与说明；不认识的是第一段。
    pub fn section(&self, id: &str) -> &'static Sec {
        let all: &'static [Sec; SECTIONS.len()] = self.sections;
        all.iter().find(|s| s.id == id).unwrap_or(&all[0])
    }
}
