//! 设置页的只读清单：子代理、MCP、技能、命令与 hook（docs/specs/rust-cli/tui-kit.md §4.2「设置」）。照抄 Puffin
//! `shell/settings/lists.rs`（两套界面不能互相依赖），类型换成旧界面自己读的（`data.rs`）。
//!
//! 与 Puffin 的不同：Puffin 在 MCP 页上 ⏎ 打开授权页、回来再问状态；这里服务器页有 Sign in 一行，按下关上设置，在 transcript
//! 里说明与给出地址（tui-manage.md §6.5）。
use rust_cli_tui_kit::app::widgets::settings::{Applies, Ink, Part, file_name, plural};

use super::data::{CommandItem, Hook, McpKind, McpServer, McpState, Scope, Skill, Subagent};
use super::page::{Act, Change, Ctx, ItemRow, Page, Row, View, Write};
use crate::ui::i18n::Lang;
use crate::ui::i18n_settings::words;

/// 下一个会话起生效的改动。
pub(super) fn next(change: Change) -> Act {
    Act::Write(Write::new(change, Applies::Next))
}

/// 值为空时说 `none` 的信息行。
pub(super) fn info_text(l: Lang, label: &str, value: &str) -> Row {
    if value.is_empty() {
        Row::info(label, vec![Part::italic(l.pick("none", "无"), Ink::Fg3)])
    } else {
        Row::info(label, vec![Part::new(value, Ink::Fg2)])
    }
}

/// 定义它的文件：⏎ 在编辑器里打开。
pub(super) fn editor(key: &str, label: &str, path: &str) -> Row {
    Row::Editor {
        key: key.to_owned(),
        label: label.to_owned(),
        path: path.to_owned(),
        desc: String::new(),
    }
}

/// 服务器的状态，行上的说法；措辞在 kit（按界面语言）。
pub(super) fn mcp_status_parts(l: Lang, s: &McpServer) -> Vec<Part> {
    use rust_cli_tui_kit::app::widgets::settings::{McpStatus, mcp_status_parts};
    let status = if !s.enabled {
        McpStatus::Off
    } else {
        match s.state {
            McpState::Connected => McpStatus::Connected { tools: s.tools },
            McpState::Failed => McpStatus::Failed,
            McpState::Disabled => McpStatus::Off,
            McpState::Connecting => McpStatus::Connecting,
            McpState::NeedsAuth => McpStatus::NeedsSignIn,
        }
    };
    mcp_status_parts(status, words(l))
}

/// `3 tools`。
fn tools_count(l: Lang, n: usize) -> String {
    match l {
        Lang::En => plural(n, "tool"),
        Lang::Zh => format!("{n} 个工具"),
    }
}

/// 用 OAuth 的服务器的授权动作（tui-manage.md §6.5）：没有令牌时
/// Sign in；有令牌时 Sign in again 与 Sign out。
fn mcp_auth_rows(l: Lang, s: &McpServer) -> Vec<Row> {
    use rust_cli_kernel_api::manage::McpAuthState;
    let opens = l.pick(
        "Opens the sign-in page in your browser.",
        "在浏览器里打开登录页。",
    );
    let sign_in = |label: &str| Row::action("signIn", label, opens, Act::McpSignIn(s.name.clone()));
    match s.auth {
        None => Vec::new(),
        Some(McpAuthState::SignedOut) => vec![sign_in(l.pick("Sign in", "登录"))],
        Some(McpAuthState::SignedIn) => {
            let said = match l {
                Lang::En => format!("Signed out of {}.", s.name),
                Lang::Zh => format!("已退出 {}。", s.name),
            };
            vec![
                sign_in(l.pick("Sign in again", "重新登录")),
                Row::action(
                    "signOut",
                    l.pick("Sign out", "退出登录"),
                    l.pick("Forgets this server's sign-in.", "忘掉这个服务器的登录。"),
                    Act::Write(
                        Write::new(
                            Change::McpSignOut {
                                name: s.name.clone(),
                            },
                            Applies::Now,
                        )
                        .saying(said),
                    ),
                ),
            ]
        }
    }
}

fn grouped_items<T>(rows: &mut Vec<Row>, title: &str, xs: &[&T], item: impl Fn(&T) -> Row) {
    if !xs.is_empty() {
        rows.push(Row::Heading(title.to_owned()));
        rows.extend(xs.iter().map(|x| item(x)));
    }
}

fn subagent_item(l: Lang, a: &Subagent) -> Row {
    let status = a
        .model
        .clone()
        .unwrap_or_else(|| l.pick("inherits the model", "继承模型").to_owned());
    Row::item(ItemRow {
        key: format!("agent:{}", a.name),
        name: a.name.clone(),
        toggle: None,
        scope: a.scope.word(l).to_owned(),
        status: vec![Part::new(status, Ink::Fg3)],
        desc: a.description.clone(),
        open: Some(Act::Push(Page::Subagent(a.name.clone()))),
        flip: None,
        detail: Vec::new(),
        remove: None,
        enter: String::new(),
        words: String::new(),
    })
}

pub fn subagents(l: Lang, all: &[Subagent]) -> Vec<Row> {
    let of = |f: fn(Scope) -> bool| all.iter().filter(|a| f(a.scope)).collect::<Vec<_>>();
    let mine = of(|s| matches!(s, Scope::User | Scope::Project));
    let mut rows = vec![Row::Heading(l.pick("Yours", "你的").to_owned())];
    if mine.is_empty() {
        rows.push(Row::Note(
            l.pick(
                "None yet. Each is a Markdown file in your agents folder.",
                "还没有。每个是你的 agents 文件夹里的一个 Markdown 文件。",
            )
            .to_owned(),
        ));
    }
    rows.extend(mine.iter().map(|a| subagent_item(l, a)));
    grouped_items(
        &mut rows,
        from_plugins(l),
        &of(|s| s == Scope::Plugin),
        |a| subagent_item(l, a),
    );
    grouped_items(
        &mut rows,
        l.pick("Built in", "内置"),
        &of(|s| s == Scope::Builtin),
        |a| subagent_item(l, a),
    );
    rows
}

fn from_plugins(l: Lang) -> &'static str {
    l.pick("From plugins", "来自插件")
}

pub fn subagent(name: &str, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(a) = ctx
        .subagents()
        .and_then(|xs| xs.iter().find(|a| a.name == name))
    else {
        return View::loading(l, l.pick("Subagent", "子代理"));
    };
    let tools = a.tools.as_ref().map_or_else(
        || l.pick("All tools", "全部工具").to_owned(),
        |ts| tools_count(l, ts.len()),
    );
    let mut rows = vec![
        info_text(l, description(l), &a.description),
        Row::info(
            l.pick("Model", "模型"),
            vec![Part::new(
                a.model
                    .clone()
                    .unwrap_or_else(|| l.pick("Inherit default", "继承默认").to_owned()),
                Ink::Fg2,
            )],
        ),
        Row::info(
            l.pick("Allowed tools", "允许的工具"),
            vec![Part::new(tools, Ink::Fg2)],
        ),
    ];
    if !a.path.is_empty() {
        rows.push(editor(
            "prompt",
            l.pick("System prompt", "系统提示词"),
            &a.path,
        ));
    }
    let desc = if a.path.is_empty() {
        l.pick("Built in: it can't be changed here.", "内置：这里改不了。")
            .to_owned()
    } else {
        ctx.tilde(&a.path)
    };
    View::new(vec![a.name.clone()], desc, rows)
}

fn description(l: Lang) -> &'static str {
    l.pick("Description", "说明")
}

pub fn mcp(l: Lang, servers: &[McpServer]) -> Vec<Row> {
    if servers.is_empty() {
        return vec![Row::Note(
            l.pick("No servers are set up.", "没有设置服务器。")
                .to_owned(),
        )];
    }
    servers
        .iter()
        .map(|s| {
            Row::item(ItemRow {
                key: format!("mcp:{}", s.name),
                name: s.name.clone(),
                toggle: Some(s.enabled),
                scope: s.scope.word(l).to_owned(),
                status: mcp_status_parts(l, s),
                desc: s.target.clone(),
                open: Some(Act::Push(Page::Mcp(s.name.clone()))),
                detail: Vec::new(),
                remove: None,
                enter: String::new(),
                words: String::new(),
                flip: Some(next(Change::McpEnabled {
                    name: s.name.clone(),
                    enabled: !s.enabled,
                })),
            })
        })
        .collect()
}

pub fn mcp_server(name: &str, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(s) = ctx.mcp().and_then(|xs| xs.iter().find(|s| s.name == name)) else {
        return View::loading(l, name);
    };
    let mut status = mcp_status_parts(l, s);
    if !s.error.is_empty() {
        status.push(Part::new(format!(" · {}", s.error), Ink::Fg3));
    }
    let mut rows = vec![Row::info(l.pick("Status", "状态"), status)];
    let (kind, where_) = match s.kind {
        McpKind::Stdio => ("stdio", command_word(l)),
    };
    rows.push(Row::info(
        l.pick("Type", "类型"),
        vec![Part::new(kind, Ink::Fg2)],
    ));
    rows.push(info_text(l, where_, &s.target));
    if let Some(m) = &s.machine {
        rows.push(Row::info(
            l.pick("Machine", "机器"),
            vec![Part::new(m.clone(), Ink::Fg2)],
        ));
    }
    rows.extend(mcp_auth_rows(l, s));
    // 要授权时重连也是同一个 401：不给 Reconnect（tui-manage.md §6.5）。
    if s.state != McpState::NeedsAuth {
        let reconnecting = match l {
            Lang::En => format!("Reconnecting {}.", s.name),
            Lang::Zh => format!("正在重连 {}。", s.name),
        };
        rows.push(Row::action(
            "reconnect",
            l.pick("Reconnect", "重连"),
            l.pick(
                "Starts the server again and reads its tools.",
                "重新启动服务器并读取它的工具。",
            ),
            Act::Write(
                Write::new(
                    Change::McpReconnect {
                        name: s.name.clone(),
                    },
                    Applies::Now,
                )
                .saying(reconnecting),
            ),
        ));
    }
    let desc = match (s.scope, l) {
        (Scope::Project, Lang::En) => concat!(
            "In this project's ",
            rust_cli_base::dot_dir!(),
            "/config.json."
        ),
        (Scope::Project, Lang::Zh) => concat!(
            "在这个项目的 ",
            rust_cli_base::dot_dir!(),
            "/config.json 里。"
        ),
        (Scope::Plugin, _) => l.pick("From a plugin.", "来自一个插件。"),
        _ => l.pick(
            "In your user config, for every project.",
            "在你的用户配置里，对每个项目都有效。",
        ),
    };
    View::new(vec![s.name.clone()], desc, rows)
}

fn command_word(l: Lang) -> &'static str {
    l.pick("Command", "命令")
}

pub fn skills(l: Lang, all: &[Skill]) -> Vec<Row> {
    let mut rows: Vec<Row> = all
        .iter()
        .map(|s| {
            Row::item(ItemRow {
                key: format!("skill:{}", s.name),
                name: s.name.clone(),
                toggle: None,
                scope: s.scope.word(l).to_owned(),
                status: vec![Part::new(s.description.clone(), Ink::Fg3)],
                desc: String::new(),
                open: Some(Act::Push(Page::Skill(s.name.clone()))),
                flip: None,
                detail: Vec::new(),
                remove: None,
                enter: String::new(),
                words: String::new(),
            })
        })
        .collect();
    rows.push(Row::plus(
        "newSkill",
        l.pick("New skill", "新技能"),
        l.pick(
            "Closes settings and asks the agent to write one with $skill-creator.",
            "关上设置，请代理用 $skill-creator 写一个。",
        ),
        Act::NewSkill,
    ));
    rows
}

pub fn skill(name: &str, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(s) = ctx
        .skills()
        .and_then(|xs| xs.iter().find(|s| s.name == name))
    else {
        return View::loading(l, l.pick("Skill", "技能"));
    };
    let mut rows = vec![info_text(l, description(l), &s.description)];
    if !s.path.is_empty() {
        rows.push(editor("file", file_name(&s.path), &s.path));
    }
    View::new(vec![s.name.clone()], ctx.tilde(&s.path), rows)
}

pub fn commands(l: Lang, all: &[CommandItem]) -> Vec<Row> {
    if all.is_empty() {
        return vec![Row::Note(
            l.pick(
                "No commands yet. Each is a Markdown file in your commands folder.",
                "还没有命令。每个是你的 commands 文件夹里的一个 Markdown 文件。",
            )
            .to_owned(),
        )];
    }
    all.iter()
        .map(|c| {
            let gap = if c.argument_hint.is_empty() { "" } else { "  " };
            let status = vec![
                Part::italic(c.argument_hint.clone(), Ink::Fg3),
                Part::new(format!("{gap}{}", c.description), Ink::Fg3),
            ]
            .into_iter()
            .filter(|p| !p.text.is_empty())
            .collect();
            Row::item(ItemRow {
                key: format!("command:{}", c.name),
                name: format!("/{}", c.name),
                toggle: None,
                scope: c.scope.word(l).to_owned(),
                status,
                desc: String::new(),
                open: Some(Act::Push(Page::Command(c.name.clone()))),
                flip: None,
                detail: Vec::new(),
                remove: None,
                enter: String::new(),
                words: String::new(),
            })
        })
        .collect()
}

pub fn command(name: &str, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(c) = ctx
        .commands()
        .and_then(|xs| xs.iter().find(|c| c.name == name))
    else {
        return View::loading(l, command_word(l));
    };
    let mut rows = vec![
        info_text(l, description(l), &c.description),
        info_text(l, l.pick("Argument hint", "参数提示"), &c.argument_hint),
    ];
    if !c.path.is_empty() {
        rows.push(editor("prompt", l.pick("Prompt", "提示词"), &c.path));
    }
    View::new(vec![format!("/{}", c.name)], ctx.tilde(&c.path), rows)
}

fn hook_item(l: Lang, i: usize, h: &Hook) -> Row {
    let gap = if h.matcher.is_empty() { "" } else { "  " };
    let status = vec![
        Part::new(h.matcher.clone(), Ink::Fg2),
        Part::new(format!("{gap}{}", h.command), Ink::Fg3),
    ]
    .into_iter()
    .filter(|p| !p.text.is_empty())
    .collect();
    Row::item(ItemRow {
        key: format!("hook:{i}"),
        name: h.event.clone(),
        toggle: None,
        scope: h.scope.word(l).to_owned(),
        status,
        desc: String::new(),
        open: Some(Act::Push(Page::Hook(i))),
        flip: None,
        detail: Vec::new(),
        remove: None,
        enter: String::new(),
        words: String::new(),
    })
}

pub fn hooks(l: Lang, all: &[Hook]) -> Vec<Row> {
    if all.is_empty() {
        return vec![Row::Note(
            l.pick("No hooks are set up.", "没有设置钩子。").to_owned(),
        )];
    }
    let mine: Vec<Row> = all
        .iter()
        .enumerate()
        .filter(|(_, h)| h.scope != Scope::Plugin)
        .map(|(i, h)| hook_item(l, i, h))
        .collect();
    let plugins: Vec<Row> = all
        .iter()
        .enumerate()
        .filter(|(_, h)| h.scope == Scope::Plugin)
        .map(|(i, h)| hook_item(l, i, h))
        .collect();
    let mut rows = mine;
    if !plugins.is_empty() {
        rows.push(Row::Heading(from_plugins(l).to_owned()));
        rows.extend(plugins);
    }
    rows
}

pub fn hook(i: usize, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(h) = ctx.hooks().and_then(|xs| xs.get(i)) else {
        return View::loading(l, l.pick("Hook", "钩子"));
    };
    let timeout = h.timeout.map(|t| format!("{t} s")).unwrap_or_default();
    let matcher = l.pick("Matcher", "匹配");
    let mut rows = vec![
        Row::info(
            l.pick("Event", "事件"),
            vec![Part::new(h.event.clone(), Ink::Fg2)],
        ),
        if h.matcher.is_empty() {
            Row::info(
                matcher,
                vec![Part::italic(l.pick("every tool", "每个工具"), Ink::Fg3)],
            )
        } else {
            info_text(l, matcher, &h.matcher)
        },
        info_text(l, command_word(l), &h.command),
        info_text(l, l.pick("Timeout", "超时"), &timeout),
    ];
    if !h.source.is_empty() {
        rows.push(editor(
            "source",
            l.pick("Config file", "配置文件"),
            &h.source,
        ));
    }
    let desc = if h.source.is_empty() {
        String::new()
    } else {
        match l {
            Lang::En => format!("In {}.", ctx.tilde(&h.source)),
            Lang::Zh => format!("在 {} 里。", ctx.tilde(&h.source)),
        }
    };
    View::new(vec![h.event.clone()], desc, rows)
}

#[cfg(test)]
#[path = "mcp_auth_tests.rs"]
mod mcp_auth_tests;
