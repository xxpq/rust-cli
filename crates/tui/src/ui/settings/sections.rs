//! 设置页的几个段自己的行：Appearance、General、Models 与 Memory（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照抄 Puffin
//! `shell/settings/page.rs` 的同名函数）。从 `page.rs` 分出（文件行数的上限）。
use std::sync::Arc;

use rust_cli_tui_kit::app::widgets::settings::{self as kit, Applies, Ink, Opt, Part};
use rust_cli_tui_kit::theme::scheme::{Ground, Scheme, ThemeChoice, ThemeMode};
use rust_cli_tui_kit::theme::schemes;

use super::data;
use super::page::{Act, Change, Ctx, ItemRow, Page, Row, TextRow, View, Write};
use crate::services::{FamilyStatus, PlanConnection, TeamStatus};
use crate::ui::i18n::Lang;

/// TUI 自己的段：三个选择，存在 `theme.json`（Puffin `appearance`）。
pub(super) fn appearance(l: Lang, look: ThemeChoice) -> Vec<Row> {
    let d = ThemeChoice::default();
    let themes = schemes::ids()
        .iter()
        .map(|id| {
            let sides = schemes::by_id(id).map_or_else(
                || l.pick("Follows your terminal", "跟随终端").to_owned(),
                Scheme::sides,
            );
            Opt::new(*id, schemes::name(id)).desc(sides)
        })
        .collect();
    vec![
        Row::Choice {
            key: "theme".to_owned(),
            label: l.pick("Theme", "主题").to_owned(),
            options: themes,
            current: look.theme.to_owned(),
            default: d.theme.to_owned(),
            desc: l
                .pick("The colours of the screen.", "屏幕的颜色。")
                .to_owned(),
            applies: Applies::Look,
            pick: Arc::new(move |v| {
                let id = schemes::ids().iter().copied().find(|id| *id == v);
                id.map_or(Act::Nothing, |theme| {
                    Act::Look(ThemeChoice { theme, ..look })
                })
            }),
        },
        Row::Choice {
            key: "themeMode".to_owned(),
            label: l.pick("Light or dark", "明暗").to_owned(),
            options: vec![
                Opt::new("auto", l.pick("Auto", "自动"))
                    .desc(l.pick("Follows your terminal", "跟随终端")),
                Opt::new("light", l.pick("Light", "浅色")),
                Opt::new("dark", l.pick("Dark", "深色")),
            ],
            current: look.mode.as_str().to_owned(),
            default: d.mode.as_str().to_owned(),
            desc: l
                .pick("Which side of the theme shows.", "主题显示哪一面。")
                .to_owned(),
            applies: Applies::Look,
            pick: Arc::new(move |v| {
                let mode = ThemeMode::parse(v).unwrap_or(d.mode);
                Act::Look(ThemeChoice { mode, ..look })
            }),
        },
        Row::Choice {
            key: "themeGround".to_owned(),
            label: l.pick("Background", "背景").to_owned(),
            options: vec![
                Opt::new("theme", l.pick("Theme's", "主题的")).desc(l.pick(
                    "The theme fills the screen and the terminal's margin",
                    "主题铺满屏幕与终端的边",
                )),
                Opt::new("terminal", l.pick("Terminal's", "终端的")).desc(l.pick(
                    "Only the theme's inks; the ground and text are the terminal's",
                    "只用主题的墨色；底色与文字是终端的",
                )),
            ],
            current: look.ground.as_str().to_owned(),
            default: d.ground.as_str().to_owned(),
            desc: l
                .pick(
                    "Who paints the screen under a named theme.",
                    "选了有名字的主题时，由谁画屏幕的底。",
                )
                .to_owned(),
            applies: Applies::Look,
            pick: Arc::new(move |v| {
                let ground = Ground::ALL
                    .into_iter()
                    .find(|g| g.as_str() == v)
                    .unwrap_or(d.ground);
                Act::Look(ThemeChoice { ground, ..look })
            }),
        },
    ]
}

/// General：Auto-compact at（读得到配置时）与界面语言（Puffin `general`）。旧界面的界面语言连屏幕一起换，说明照实写。
/// 最后是 ← 打开任务列表的开关（本工程才有，tui-cockpit.md §3.6）；`left_arrow` 是它现在的值。
pub(super) fn general(l: Lang, g: &data::General, left_arrow: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(at) = g.auto_compact {
        let mut t = TextRow::new(
            "autoCompact",
            l.pick("Auto-compact at", "自动压缩于"),
            at.map(super::tokens::exact).unwrap_or_default(),
            l.pick(
                "Compact the conversation once it holds this many tokens. A model with a smaller window compacts at its own limit.",
                "对话达到这么多 token 时压缩。窗口更小的模型按它自己的上限压缩。",
            ),
            Applies::Now,
            move |v| {
                if v.trim().is_empty() {
                    Act::Write(Write::new(Change::AutoCompact { at: None }, Applies::Now))
                } else {
                    super::tokens::parse(v).map_or_else(
                        || {
                            Act::Refuse(
                                l.pick(
                                    "Write a number of tokens, like 400k.",
                                    "写一个 token 数，例如 400k。",
                                )
                                .to_owned(),
                            )
                        },
                        |n| {
                            Act::Write(Write::new(
                                Change::AutoCompact { at: Some(n) },
                                Applies::Now,
                            ))
                        },
                    )
                }
            },
        );
        t.unit = "tokens".to_owned();
        t.placeholder = "400k".to_owned();
        t.empty = l.pick("model limit", "模型的上限").to_owned();
        rows.push(Row::Text(t));
    }
    rows.push(Row::Choice {
        key: "locale".to_owned(),
        label: l.pick("Language", "语言").to_owned(),
        options: vec![
            Opt::new("auto", l.pick("Auto", "自动"))
                .desc(l.pick("Follows your system", "跟随系统")),
            Opt::new("en-US", "English"),
            Opt::new("zh-CN", "简体中文"),
        ],
        current: g.locale.clone(),
        default: "auto".to_owned(),
        // 旧界面有中文界面：屏幕跟着换（Puffin 写 “The screen stays in English for now.”）。
        desc: l
            .pick(
                "The language of the screen and of the agent's reminders.",
                "屏幕与代理的提醒用的语言。",
            )
            .to_owned(),
        applies: Applies::Now,
        pick: Arc::new(|v| {
            Act::Write(Write::new(
                Change::Locale {
                    locale: v.to_owned(),
                },
                Applies::Now,
            ))
        }),
    });
    rows.push(Row::Switch {
        key: "leftArrowOpensCockpit".to_owned(),
        label: l.pick("← opens the task list", "← 打开任务列表").to_owned(),
        on: left_arrow,
        desc: l
            .pick(
                "On an empty prompt, ← opens the task list. /cockpit opens it either way.",
                "输入框空着时，← 打开任务列表。/cockpit 总能打开它。",
            )
            .to_owned(),
        applies: Applies::Now,
        set: Arc::new(Act::LeftArrow),
    });
    rows
}

/// Models：缺省模型、内置的家族（账号端口给的）、模型目录里的 provider 与添加（Puffin `models`）。provider 的页、新
/// provider 与模型页在 `providers.rs`；开关 provider 经 runner 写，runner 回答这一版还没有，同 Puffin 的 host
/// （tui-kit.md §4.5）。
pub(super) fn models(l: Lang, m: &data::Models) -> Vec<Row> {
    let mut rows = vec![
        Row::Picker {
            key: "defaultModel".to_owned(),
            label: default_model(l).to_owned(),
            value: m.default_model.clone(),
            desc: l
                .pick(
                    "The model new sessions start with. /model changes only one session.",
                    "新会话起始用的模型。/model 只改一个会话。",
                )
                .to_owned(),
        },
        Row::Heading(l.pick("Providers", "服务商").to_owned()),
    ];
    rows.extend(m.families.iter().map(|f| {
        Row::item(ItemRow {
            key: format!("family:{}", f.family.as_str()),
            name: f.name.clone(),
            status: family_status(l, f),
            desc: match l {
                Lang::En => format!("Built in. Sign in with your {} account.", f.name),
                Lang::Zh => format!("内置。用你的 {} 账号登录。", f.name),
            },
            open: Some(Act::Push(Page::Family(f.family.as_str().to_owned()))),
            ..ItemRow::default()
        })
    }));
    rows.push(Row::Heading(
        l.pick("Your providers", "你的服务商").to_owned(),
    ));
    rows.extend(m.providers.iter().map(|p| {
        Row::item(ItemRow {
            key: format!("provider:{}", p.id),
            name: p.name.clone(),
            toggle: Some(true),
            status: vec![Part::new(models_count(l, p.models.len()), Ink::Fg3)],
            open: Some(Act::Push(Page::Provider(p.id.clone()))),
            flip: Some(super::providers::edit(
                &p.id,
                super::page::ProviderEdit::Enabled(false),
            )),
            ..ItemRow::default()
        })
    }));
    rows.push(Row::plus(
        "addProvider",
        add_provider(l),
        l.pick(
            "Any service that speaks one of three APIs: chat completions, responses, or Anthropic messages.",
            "任何讲这三种 API 之一的服务：chat completions、responses 或 Anthropic messages。",
        ),
        Act::Push(Page::NewProvider),
    ));
    rows
}

/// 缺省模型的行名（段里的行与输入框上的选择器共用）。
pub(super) fn default_model(l: Lang) -> &'static str {
    l.pick("Default model", "默认模型")
}

pub(super) fn add_provider(l: Lang) -> &'static str {
    l.pick("Add provider", "添加服务商")
}

/// `3 models`。
fn models_count(l: Lang, n: usize) -> String {
    match l {
        Lang::En => kit::plural(n, "model"),
        Lang::Zh => format!("{n} 个模型"),
    }
}

/// Memory：规则文件，⏎ 在编辑器里打开；不存在的打开时先建空文件（Puffin `memory`）。
pub(super) fn memory(files: &[data::MemoryFile], ctx: &Ctx<'_>) -> Vec<Row> {
    let l = ctx.lang;
    if files.is_empty() {
        return vec![Row::Note(
            l.pick(
                "No instruction files are known here.",
                "这里没有已知的规则文件。",
            )
            .to_owned(),
        )];
    }
    files
        .iter()
        .map(|f| {
            let status = if f.exists {
                kit::size(f.size)
            } else {
                l.pick("not created yet", "还没建").to_owned()
            };
            Row::item(ItemRow {
                key: format!("mem:{}", f.path),
                name: kit::file_name(&f.path).to_owned(),
                scope: super::data::scope_word(l, &f.scope),
                status: vec![Part::new(status, Ink::Fg3)],
                desc: ctx.tilde(&f.path),
                open: Some(Act::Edit(f.path.clone())),
                ..ItemRow::default()
            })
        })
        .collect()
}

/// 家族的计划在 Connection 行里的名字（Puffin `connection_name`、`team_name`）。
pub(crate) fn plan_name(f: &FamilyStatus) -> String {
    match f.connection {
        PlanConnection::Start => "Start Plan".to_owned(),
        PlanConnection::Individual => "Individual Plan".to_owned(),
        PlanConnection::Team(i) => f
            .teams
            .get(i)
            .map_or_else(|| "Team Plan".to_owned(), team_name),
    }
}

fn team_name(t: &TeamStatus) -> String {
    if t.name.is_empty() {
        "Team Plan".to_owned()
    } else {
        format!("Team Plan · {}", t.name)
    }
}

/// 家族行的状态（Puffin `family_status`）。
fn family_status(l: Lang, f: &FamilyStatus) -> Vec<Part> {
    if !f.signed_in {
        return vec![Part::new(l.pick("not signed in", "未登录"), Ink::Fg3)];
    }
    vec![
        Part::new(l.pick("signed in", "已登录"), Ink::Ok),
        Part::new(format!(" · {}", plan_name(f)), Ink::Fg3),
    ]
}

/// 一个家族的页：Connection、账号与它的模型（Puffin `family`）。换计划经 runner 写，两边的 host 都回答这一版还没有
/// （tui-kit.md §4.5）；Sign out 是旧界面的 `/logout`。
pub(super) fn family(id: &str, ctx: &Ctx<'_>) -> View {
    let l = ctx.lang;
    let Some(f) = ctx
        .models()
        .and_then(|m| m.families.iter().find(|f| f.family.as_str() == id))
    else {
        return View::loading(l, id);
    };
    let s = f;
    let teams: Vec<Opt> = if s.teams.is_empty() {
        let why = if !s.signed_in {
            l.pick("Sign in to see your teams", "登录后才能看到你的团队")
                .to_owned()
        } else if !s.teams_error.is_empty() {
            match l {
                Lang::En => format!("Could not read your teams: {}", s.teams_error),
                Lang::Zh => format!("没能读到你的团队：{}", s.teams_error),
            }
        } else {
            l.pick("This account has no team plan", "这个账号没有团队计划")
                .to_owned()
        };
        vec![Opt::new("team-coding-plan", "Team Plan").disabled(why)]
    } else {
        s.teams
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let why = if t.usable {
                    String::new()
                } else if !t.why.is_empty() {
                    t.why.clone()
                } else {
                    l.pick("Its key is not available yet", "它的密钥还拿不到")
                        .to_owned()
                };
                Opt::new(format!("team:{i}"), team_name(t)).disabled(why)
            })
            .collect()
    };
    let mut options = vec![
        Opt::new("start-plan", "Start Plan").desc(l.pick("Free", "免费")),
        Opt::new("individual-coding-plan", "Individual Plan"),
    ];
    options.extend(teams);
    let current = match s.connection {
        PlanConnection::Start => "start-plan".to_owned(),
        PlanConnection::Individual => "individual-coding-plan".to_owned(),
        PlanConnection::Team(i) => format!("team:{i}"),
    };
    let family = s.family;
    let mut rows = vec![Row::Choice {
        key: "connection".to_owned(),
        label: l.pick("Connection", "连接方式").to_owned(),
        options,
        current,
        default: String::new(),
        desc: l
            .pick(
                "How ZCode pays for your requests.",
                "ZCode 怎样为你的请求付费。",
            )
            .to_owned(),
        applies: Applies::Next,
        pick: Arc::new(move |v| {
            super::providers::next(Change::Connection {
                family,
                plan: v.to_owned(),
            })
        }),
    }];
    if s.signed_in {
        let who = if s.account.is_empty() {
            l.pick("signed in", "已登录")
        } else {
            s.account.as_str()
        };
        rows.push(Row::info(
            l.pick("Account", "账号"),
            vec![Part::new(who, Ink::Fg2)],
        ));
        rows.push(Row::action(
            "signOut",
            l.pick("Sign out", "退出登录"),
            l.pick(
                "The desktop app is signed out too: they share one account.",
                "桌面应用也会退出：它们共用一个账号。",
            ),
            Act::Write(Write::new(Change::SignOut, Applies::Next).saying(l.pick(
                "Signed out. The desktop app is signed out too.",
                "已退出登录。桌面应用也退出了。",
            ))),
        ));
    } else {
        let desc = match l {
            Lang::En => format!(
                "Opens {} in your browser. Sign in there; this page changes by itself.",
                s.name
            ),
            Lang::Zh => format!("在浏览器里打开 {}。在那里登录；这一页会自己更新。", s.name),
        };
        rows.push(Row::action(
            "signIn",
            l.pick("Sign in", "登录"),
            &desc,
            Act::SignIn(s.family),
        ));
    }
    rows.push(Row::Heading(l.pick("Models", "模型").to_owned()));
    if f.models.is_empty() {
        let note = if s.signed_in {
            l.pick("No models.", "没有模型。")
        } else {
            l.pick("Sign in to see your models.", "登录后才能看到你的模型。")
        };
        rows.push(Row::Note(note.to_owned()));
    } else {
        rows.extend(f.models.iter().map(|m| {
            super::providers::model_item(
                l,
                &s.provider,
                m,
                l.pick(
                    "Turned-off models stay out of /model and the default model list.",
                    "停用的模型不出现在 /model 与默认模型的列表里。",
                ),
            )
        }));
    }
    View::new(
        vec![s.name.clone()],
        l.pick(
            "Built-in provider. Your account decides which models you can use.",
            "内置的服务商。你的账号决定能用哪些模型。",
        ),
        rows,
    )
}
