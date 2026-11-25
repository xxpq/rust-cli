//! 设置页里 kit 写的字的中文（docs/specs/rust-cli/tui-kit.md §4.5「语言」，用户决定 2026-11-12）：段名与说明、值的说法、
//! 生效的时刻、键的说明、占位与 MCP 的状态。英文是 kit 的 [`SETTINGS_WORDS`]（与以前一字不差）；中文是这里的译文。
//! 行自己的字由 `settings/*` 按 `Ctx.lang` 写。
use rust_cli_tui_kit::app::widgets::settings::hints::Asking;
use rust_cli_tui_kit::app::widgets::settings::{
    Applies, McpStatus, SECTIONS, SETTINGS_WORDS, Sec, SettingsWords,
};

use super::i18n::Lang;

pub(crate) fn words(lang: Lang) -> &'static SettingsWords {
    match lang {
        Lang::En => &SETTINGS_WORDS,
        Lang::Zh => &ZH_WORDS,
    }
}

/// 段：id 与组同 [`SECTIONS`]（测试核对），名字与说明是中文。
const ZH_SECTIONS: [Sec; SECTIONS.len()] = [
    Sec {
        id: "general",
        name: "常规",
        group: 0,
        desc: "代理怎样工作。这些与桌面应用的设置是同一份。",
    },
    Sec {
        id: "permissions",
        name: "权限",
        group: 0,
        desc: "代理不问你就能做的事。其余由你的模式决定；这些规则是例外，“从不运行”的规则压过其他规则。",
    },
    Sec {
        id: "models",
        name: "模型",
        group: 0,
        desc: "代理的模型从哪里来。这里的改动从新会话起生效。",
    },
    Sec {
        id: "memory",
        name: "记忆",
        group: 0,
        desc: "代理在会话开始时读的规则文件。",
    },
    Sec {
        id: "appearance",
        name: "外观",
        group: 0,
        desc: "ZCode 在这个终端里的样子。只有 TUI 有这些，存在 theme.json 里。",
    },
    Sec {
        id: "subagents",
        name: "子代理",
        group: 1,
        desc: "代理可以把任务交给它们。每个是一个 Markdown 文件。",
    },
    Sec {
        id: "mcp",
        name: "MCP",
        group: 1,
        desc: "代理能调用的工具服务器。一个服务器失败不会让代理停下，只是少了它的工具。",
    },
    Sec {
        id: "machines",
        name: "机器",
        group: 1,
        desc: "代理能经 SSH 在上面运行工具的电脑。连上的机器在每个会话里都提供给代理的工具；■ 表示已连接。",
    },
    Sec {
        id: "skills",
        name: "技能",
        group: 1,
        desc: "任务需要时代理加载的说明。在消息里用 $名字 提到一个。",
    },
    Sec {
        id: "commands",
        name: "命令",
        group: 1,
        desc: "你的斜杠命令。每个是一个 Markdown 文件，它的文字就是代理收到的。",
    },
    Sec {
        id: "hooks",
        name: "钩子",
        group: 1,
        desc: "会话里发生某件事时运行的命令。它们在沙箱外运行。",
    },
    Sec {
        id: "status",
        name: "状态",
        group: 2,
        desc: "这个会话：它是什么、在哪里运行、连着什么。",
    },
    Sec {
        id: "context",
        name: "上下文",
        group: 2,
        desc: "每次请求发给模型的内容，以及它占了窗口的多少。",
    },
    Sec {
        id: "stats",
        name: "统计",
        group: 2,
        desc: "你在这台电脑上的使用情况，来自本地的记录。",
    },
];

/// 状态栏的键的说明：kit 与设置页各处的英文词。
fn zh_key(word: &str) -> Option<&'static str> {
    Some(match word {
        "cancel" => "取消",
        "save" => "保存",
        "choose" => "选择",
        "pick" => "选定",
        "back" => "返回",
        "answer" => "回答",
        "keep" => "保留",
        "model" => "模型",
        "effort" => "档位",
        "go there" => "前往",
        "clear" => "清除",
        "section" => "段",
        "sections" => "段目录",
        "open" => "打开",
        "answer it" => "去回答",
        "close" => "关闭",
        "turn off" => "停用",
        "turn on" => "启用",
        "remove" => "删除",
        "edit" => "编辑",
        "use it" => "使用",
        "add a rule" => "添加规则",
        "delete" => "删除",
        "add" => "添加",
        "run" => "运行",
        "change" => "更改",
        "expand" => "展开",
        "collapse" => "收起",
        "scroll" => "滚动",
        "copy id" => "复制 ID",
        "details" => "详情",
        "range" => "范围",
        "connect" => "连接",
        "disconnect" => "断开",
        "stop" => "停止",
        "retry now" => "立即重试",
        "try again" => "再试一次",
        _ => return None,
    })
}

const ZH_WORDS: SettingsWords = SettingsWords {
    sections: &ZH_SECTIONS,
    title: "设置",
    matches: |n| format!("{n} 处匹配"),
    on: "开",
    off: "关",
    default: "默认",
    not_set: "未设置",
    current: "当前",
    keep_it: "保留",
    signing_in: "已在浏览器里打开登录页，请在那里登录。",
    waiting: "等你登录",
    testing: "测试中…",
    connected: "已连接",
    answered: |s| format!(" · {s:.1} 秒内回答"),
    opens_in: |editor| {
        if editor.is_empty() {
            "⏎ 在你的编辑器里打开它。".to_owned()
        } else {
            format!("⏎ 在 {editor} 里打开它。")
        }
    },
    above: |n| format!("↑ 还有 {n} 行"),
    below: |n| format!("↓ 还有 {n} 行"),
    applies: |a| match a {
        Applies::Now => "立即生效。",
        Applies::Next => "新会话起生效。",
        Applies::Form => "",
        Applies::Look => "立即显示。",
    },
    saved: |a| match a {
        Applies::Now => "已保存。代理从现在起按它做。",
        Applies::Next => "已保存。新会话按它做；这个会话照旧。",
        Applies::Form => "",
        Applies::Look => "已保存，已经显示。",
    },
    asking: |a| match a {
        Asking::Question => "代理有一个问题。",
        Asking::Plan => "代理有一份计划给你。",
        Asking::Permission => "代理在请求权限。",
    },
    goes_back: " 回去回答。",
    key: zh_key,
    edit_in: |editor| format!("在 {editor} 里编辑"),
    search: "搜索设置",
    type_text: "输入文字",
    type_key: "输入或粘贴密钥",
    type_value: "输入一个值",
    hidden: "输入时隐藏",
    mcp: |s| match s {
        McpStatus::Off => "关",
        McpStatus::NeedsSignIn => "需要登录",
        McpStatus::Connected { .. } => "已连接",
        McpStatus::Failed => "失败",
        McpStatus::Untrusted => "需要审核",
        McpStatus::Disconnected => "已断开",
        McpStatus::Connecting => "连接中",
    },
    tools: |n| format!("{n} 个工具"),
};

#[cfg(test)]
mod tests {
    use super::*;

    /// 中文的段与 kit 的段同序、同 id、同组：只换名字与说明。
    #[test]
    fn chinese_sections_match_the_kit() {
        for (zh, en) in ZH_SECTIONS.iter().zip(SECTIONS.iter()) {
            assert_eq!((zh.id, zh.group), (en.id, en.group));
        }
    }
}
