//! 设置的 Permissions 段的中文措辞（docs/specs/rust-cli/tui-kit.md §4.5「语言」，用户决定 2026-11-12）：kit
//! `settings::rules` 的英文说法按英文的语法拼（名词、动词、动名词），不能逐词替换，所以中文在这里另写一份，函数与 kit 的
//! 一一对应；规则的原文（命令前缀、路径、主机、工具名）不译。
use rust_cli_kernel_api::tool_names::{BASH, EDIT, READ, WEB_FETCH, WEB_SEARCH};
use rust_cli_tui_kit::app::widgets::settings::rules::{Behavior, Rule, Source, prefix};
use rust_cli_tui_kit::app::widgets::settings::{Ink, Opt, Part};

/// 三组的标题，次序同 kit 的 `GROUPS`。
pub fn group(b: Behavior) -> &'static str {
    match b {
        Behavior::Allow => "直接运行",
        Behavior::Ask => "总要询问",
        Behavior::Deny => "从不运行",
    }
}

/// 规则什么时候生效。
const EFFECT: &str = "从下一次工具调用起";
pub const EMPTY: &str = "这里还没有规则。在请求上选“总是允许”，它的规则就出现在这里。";
pub const ADD: &str = "为这个项目允许、询问或拒绝一个命令、一个文件或一个网站。";
pub const NEW_DESC: &str = "这个项目的一条规则，同“总是允许”加的那些一样存在这台电脑上。输入一个命令、一个网址，或像 Edit(src/**) 这样的规则。";
pub const RULE_HINT: &str = "一个命令、一个网址，或 工具(说明)";
pub const NEEDS_RULE: &str = "需要一条规则。";
pub const NO_HOST: &str = "这个地址没有主机";
pub const NO_CLOSE: &str = "规则少了结尾的 )";
pub const OPEN_FILE: &str = "打开文件";
pub const FOLDERS: &str = "它工作的文件夹";

/// `mcp__服务器__工具` 的服务器与工具（同 kit）。
fn mcp_parts(tool: &str) -> Option<(&str, Option<&str>)> {
    let rest = tool.strip_prefix("mcp__")?;
    Some(match rest.split_once("__") {
        Some((server, "*")) => (server, None),
        Some((server, tool)) => (server, Some(tool)),
        None => (rest, None),
    })
}

fn host(content: &str) -> &str {
    content.strip_prefix("domain:").unwrap_or(content)
}

pub fn behavior_name(b: Behavior) -> &'static str {
    group(b)
}

pub fn source_word(s: Source) -> &'static str {
    match s {
        Source::UserSettings => "用户",
        Source::ProjectSettings => "项目",
        Source::LocalSettings => "本项目",
        Source::CliArg => "命令行",
        Source::Session => "本会话",
    }
}

pub fn kind_word(tool: &str) -> &'static str {
    match tool {
        BASH => "命令",
        EDIT => "编辑",
        READ => "读取",
        WEB_FETCH => "网页",
        WEB_SEARCH => "网页搜索",
        t if t.starts_with("mcp__") => "MCP 工具",
        _ => "工具",
    }
}

/// 规则的说法（kit `phrase`）：命令是前缀加 ` …`。
pub fn phrase(rule: Rule<'_>) -> String {
    match (rule.tool, rule.content) {
        (BASH, None) => "任何命令".to_owned(),
        (BASH, Some(c)) => prefix(c).map_or_else(|| c.to_owned(), |p| format!("{p} …")),
        (EDIT, Some(c)) => format!("编辑 {c}"),
        (EDIT, None) => "任何编辑".to_owned(),
        (READ, Some(c)) => format!("读取 {c}"),
        (READ, None) => "任何读取".to_owned(),
        (WEB_FETCH, Some(c)) => format!("{} 上的网页", host(c)),
        (WEB_FETCH, None) => "任何网页".to_owned(),
        (WEB_SEARCH, _) => "任何网页搜索".to_owned(),
        (tool, _) => match mcp_parts(tool) {
            Some((server, Some(t))) => format!("{server} · {t}"),
            Some((server, None)) => server.to_owned(),
            None => rule.text.to_owned(),
        },
    }
}

/// 说法去掉前缀的 ` …`。
fn short(rule: Rule<'_>) -> String {
    let p = phrase(rule);
    p.strip_suffix(" …")
        .map_or_else(|| p.clone(), str::to_owned)
}

/// 名词（kit `noun`）。
fn noun(rule: Rule<'_>) -> String {
    match (rule.tool, rule.content) {
        (BASH, Some(c)) => {
            prefix(c).map_or_else(|| format!("命令 {c}"), |p| format!("以 {p} 开头的任何命令"))
        }
        _ => phrase(rule),
    }
}

/// 带墨的名词（添加页的 Means）：具体的部分 fg，其余 fg3。
pub fn means(rule: Rule<'_>) -> Vec<Part> {
    let fg3 = |t: &str| Part::new(t, Ink::Fg3);
    let fg = |t: &str| Part::new(t, Ink::Fg);
    match (rule.tool, rule.content) {
        (BASH, Some(c)) => match prefix(c) {
            Some(p) => vec![fg3("以 "), fg(p), fg3(" 开头的命令")],
            None => vec![fg3("命令 "), fg(c)],
        },
        (EDIT, Some(c)) => vec![fg3("编辑 "), fg(c)],
        (READ, Some(c)) => vec![fg3("读取 "), fg(c)],
        (WEB_FETCH, Some(c)) => vec![fg(host(c)), fg3(" 上的网页，任何路径")],
        (tool, _) => match mcp_parts(tool) {
            Some((server, Some(t))) => vec![fg(server), fg3(" 的工具 "), fg(t)],
            Some((server, None)) => vec![fg(server), fg3(" 的任何工具")],
            None if rule.content.is_none() && kind_word(tool) == "工具" => {
                vec![fg3("任何对 "), fg(tool), fg3(" 的使用")]
            }
            None => vec![fg3(&phrase(rule))],
        },
    }
}

/// 动词短语（kit `verb`）。
fn verb(rule: Rule<'_>) -> String {
    match (rule.tool, rule.content) {
        (BASH, None) => "运行任何命令".to_owned(),
        (BASH, Some(c)) => prefix(c).map_or_else(
            || format!("运行 {c}"),
            |p| format!("运行以 {p} 开头的任何命令"),
        ),
        (EDIT, Some(c)) => format!("编辑 {c}"),
        (EDIT, None) => "编辑任何文件".to_owned(),
        (READ, Some(c)) => format!("读取 {c}"),
        (READ, None) => "读取任何文件".to_owned(),
        (WEB_FETCH, Some(c)) => format!("获取 {} 上的网页", host(c)),
        (WEB_FETCH, None) => "获取任何网页".to_owned(),
        (WEB_SEARCH, _) => "搜索网页".to_owned(),
        (tool, _) if tool.starts_with("mcp__") => format!("调用 {}", phrase(rule)),
        _ => format!("使用 {}", rule.text),
    }
}

/// 选中的规则下面说它做什么。
pub fn sentence(behavior: Behavior, rule: Rule<'_>) -> String {
    let what = noun(rule);
    match behavior {
        Behavior::Allow => format!("允许{what}，不询问。"),
        Behavior::Ask => format!("{what}之前先询问。"),
        Behavior::Deny => format!("拒绝{what}。"),
    }
}

/// 规则页的说明。
pub fn page_desc(behavior: Behavior, rule: Rule<'_>) -> String {
    let v = verb(rule);
    match behavior {
        Behavior::Allow => format!("让代理{v}，不询问。"),
        Behavior::Ask => format!("不论模式，代理{v}之前都先询问。"),
        Behavior::Deny => format!("不让代理{v}；这条压过其他规则。"),
    }
}

/// 删之前问的那句。
pub fn removal_note(behavior: Behavior, rule: Rule<'_>) -> String {
    match behavior {
        Behavior::Allow => format!("代理在{}之前会重新询问。", short(rule)),
        _ => format!("{}重新由你的模式决定。", short(rule)),
    }
}

/// 删掉之后输入框上方说的。
pub fn removed_said(behavior: Behavior, rule: Rule<'_>) -> String {
    match behavior {
        Behavior::Allow => format!("已删除。{EFFECT}，代理在{}之前重新询问。", short(rule)),
        _ => format!("已删除。{EFFECT}，{}重新由你的模式决定。", short(rule)),
    }
}

pub fn replaced_said() -> String {
    format!("已保存。{EFFECT}，代理按它做。")
}

pub fn added_said() -> String {
    format!("已添加。{EFFECT}，代理按它做。")
}

/// 种类的选项。
pub fn kind_options() -> Vec<Opt> {
    vec![
        Opt::new("allow", group(Behavior::Allow)).desc("不先询问"),
        Opt::new("ask", group(Behavior::Ask)).desc("不论模式，每次都问"),
        Opt::new("deny", group(Behavior::Deny)).desc("拒绝；压过其他规则"),
    ]
}

/// 配置文件的规则选中时说它从哪里来。
pub fn config_desc(file: Option<&str>) -> String {
    match file {
        Some(f) => format!("来自 {f}，ZCode 只读它。"),
        None => "来自一个配置文件，ZCode 只读它。".to_owned(),
    }
}

/// 规则页的 Kept：本项目的规则存在哪里。
pub fn kept_local(place: Option<&str>) -> String {
    format!("为 {}，存在这台电脑上", place.unwrap_or("这个项目"))
}

/// 规则页的 Kept：命令行与会话的规则。
pub fn kept_other(source: Source) -> &'static str {
    if source == Source::CliArg {
        "只在这次运行，来自命令行"
    } else {
        "只在这个会话"
    }
}

/// 一个工作的文件夹的来源列与说明。
pub fn folder_words(added: bool) -> (&'static str, &'static str) {
    if added {
        ("--add-dir", "ZCode 启动时用 --add-dir 加的。")
    } else {
        ("工作文件夹", "代理在这里工作。")
    }
}
