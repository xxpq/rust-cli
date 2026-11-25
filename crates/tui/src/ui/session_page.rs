//! 设置的 Status 段（docs/specs/rust-cli/tui-panels.md §4.5；以前是统计的页的 Session 标签）：这个会话是什么、在哪里跑、接上
//! 了什么。v1 `/status` 的六行（版本、模型、目录、权限、会话 ID、MCP）与更多的事实：开始的时刻与时长、窗口与压缩的位置、账号与
//! 计划、分支、新版本、每个 MCP 服务器、机器、规则文件、hooks。
//!
//! 数据：界面已有的（会话、标题、版本、视图的模型与模式、机器、规则文件）画的时候读；窗口读上下文的计数（`App.context_count`，
//! 同 Context 段）；MCP、hooks、账号与分支是一次问的事实（[`SessionFacts`]，runner 的 `session_facts`），Status 显示时打开、换到
//! 它与每次模型请求结束时问，同时只有一次在途。标签第 3 列、值第 14 列（页框的左边距在设置的窗格里去掉，`settings/live.rs`）；
//! 句子随界面语言（2026-11-12 起，同设置页，tui-kit.md §4.5「语言」）。
use rust_cli_kernel_api::context_usage::{ContextUsage, WindowSource};
use rust_cli_kernel_api::manage::{HookEntry, McpServerInfo, McpServerState};
use rust_cli_kernel_api::panels::SubagentStatus;
use rust_cli_tui_kit::app::widgets::fit::{MEASURE_MARGIN, MEASURE_MAX, clip, count, pad};
use rust_cli_tui_kit::app::widgets::stats;
use rust_cli_tui_kit::app::widgets::usage::view::{LEAD, clock, month_day, spread};
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::App;
use super::context_page::{Row, sp, window_of};
use super::i18n::Lang;
use super::settings::live::Live;
use crate::machines::{MachineRow, MachineStatus};
use crate::services::FamilyStatus;

/// 标签从第 3 列起、补到 11 列；值从第 14 列起（同卡的 Context 行）。
const LABEL_AT: usize = 3;
const LABEL: usize = 11;
const VALUE_AT: usize = 14;
/// 分段的行：记号第 3 列，名字第 5 列；第二列在这一段最长的名字之后 2 列，至少第 19 列、至多行宽的一半（[`second_at`]）；
/// 工具数右对齐 2 列。
const MARK_AT: usize = 3;
const NAME_AT: usize = 5;
const SECOND_AT: usize = 19;
const NAME_GAP: usize = 2;
const TOOLS_WIDTH: usize = 2;
/// 不到这个宽度时：ID 只写头尾、不写新版本的提示与行尾的指向。
const MEDIUM: usize = 76;
/// 短的 ID 留头几个与尾几个字符。
const ID_HEAD: usize = 8;
const ID_TAIL: usize = 6;
/// hook 的事件名补到最长的加 2 列，至多 22 列。
const EVENT_GAP: usize = 2;
const EVENT_MAX: usize = 22;
/// 规则文件的种类离路径 3 列；分段的标题与右端的数之间至少空 3 列（同 `/usage` 的 `headed`）。
const KIND_GAP: usize = 3;
const HEADING_GAP: i64 = 3;
/// 行尾的指向（`/mcp`、`/machines`）离右边 3 列；原因与它之间至少空 2 列。
const POINTER_MARGIN: usize = 3;
const POINTER_GAP: usize = 2;
/// 一天的毫秒数：开始的时刻不在今天时写日期。
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// 一次问的事实（§4.5）：MCP 服务器、hooks、内置家族的登录状态、模型目录里的 provider 名字与工作区的分支。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFacts {
    pub mcp: Vec<McpServerInfo>,
    pub hooks: Vec<HookEntry>,
    pub families: Vec<FamilyStatus>,
    /// （provider 的 ID，名字）。自己加的 provider 的 ID 是 UUID，只有名字能写出来。
    pub providers: Vec<(String, String)>,
    pub branch: Option<String>,
}

/// 复制之后槽里说的（到下一个键）：整页（`c`、设置页的 ⌃Y），或 Status 上 ⏎ 的会话 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    Page,
    SessionId,
}

/// `a  ·  b` 的点。
fn dot(theme: &Theme) -> Span {
    sp("  ·  ", theme.line)
}

/// 一行 `标签  值`：标签 fg2，值从第 14 列起。
fn labeled(theme: &Theme, label: &str, value: Vec<Span>) -> StyledLine {
    Row::default()
        .at(LABEL_AT, vec![sp(pad(label, LABEL), theme.fg2)])
        .at(VALUE_AT, value)
        .line()
}

/// 值的第二行（从第 14 列起）。
fn more(value: Vec<Span>) -> StyledLine {
    Row::default().at(VALUE_AT, value).line()
}

/// 分段的标题：第 3 列 fg2 粗体，右端 fg3 的几个数（同 `/usage` 的 `headed`）。
fn headed(theme: &Theme, cols: usize, text: &str, right: Vec<Span>) -> StyledLine {
    let left = vec![Span::plain(LEAD), sp(text, theme.fg2.with_attr(attr::BOLD))];
    StyledLine::new(spread(left, right, cols, HEADING_GAP))
}

/// 标题右端的数：`3 connected  ·  1 failed`，只写不为 0 的；`err` 的那一项数是 err 色。
fn tally(theme: &Theme, parts: &[(usize, &str, bool)]) -> Vec<Span> {
    let mut out = Vec::new();
    for (n, word, err) in parts.iter().filter(|(n, _, _)| *n > 0) {
        if !out.is_empty() {
            out.push(dot(theme));
        }
        let ink = if *err {
            Style::fg(theme.err)
        } else {
            theme.fg2
        };
        out.push(sp(n.to_string(), ink));
        out.push(sp(format!(" {word}"), theme.fg3));
    }
    out
}

/// 单数或复数：`1 file`、`2 files`；中文是 `1 个文件`（`zh` 是量词与名词）。
fn plural(l: Lang, n: usize, one: &str, many: &str, zh: &str) -> String {
    match l {
        Lang::En => format!("{n} {}", if n == 1 { one } else { many }),
        Lang::Zh => format!("{n} {zh}"),
    }
}

/// 一段的第二列从哪一列起：最长的名字之后 2 列，至少 `least`、至多行宽的一半。
fn second_at<'a>(cols: usize, least: usize, names: impl Iterator<Item = &'a str>) -> usize {
    let longest = names.map(string_width).max().unwrap_or(0);
    (NAME_AT + longest + NAME_GAP).clamp(least, (cols / 2).max(least))
}

/// 分段的一行：记号、名字，第二列从 `at` 起（[`second_at`]）；有指向时写在行尾，第二列让出位置。
fn item(
    cols: usize,
    at: usize,
    mark: Span,
    name: Span,
    second: Vec<Span>,
    pointer: Option<(&str, Style)>,
) -> StyledLine {
    let name = Span::new(clip(&name.text, at.saturating_sub(NAME_AT + 1)), name.style);
    let mut row = Row::default()
        .at(MARK_AT, vec![mark])
        .at(NAME_AT, vec![name]);
    let pointer = pointer.filter(|_| cols >= MEDIUM);
    let end = match pointer {
        Some((p, _)) => cols.saturating_sub(POINTER_MARGIN + string_width(p) + POINTER_GAP),
        None => cols.saturating_sub(1),
    };
    let room = end.saturating_sub(at);
    let mut left = room;
    let mut clipped = Vec::new();
    for s in second {
        if left == 0 {
            break;
        }
        let text = clip(&s.text, left);
        left = left.saturating_sub(string_width(&text));
        clipped.push(Span::new(text, s.style));
    }
    row = row.at(at, clipped);
    if let Some((p, style)) = pointer {
        row = row.right(cols.saturating_sub(POINTER_MARGIN), vec![sp(p, style)]);
    }
    row.line()
}

/// 完整的 ID，窄时只写头尾。
fn id_text(id: &str, cols: usize) -> String {
    let chars: Vec<char> = id.chars().collect();
    if cols >= MEDIUM || chars.len() <= ID_HEAD + ID_TAIL + 1 {
        return id.to_owned();
    }
    let head: String = chars[..ID_HEAD].iter().collect();
    let tail: String = chars[chars.len() - ID_TAIL..].iter().collect();
    format!("{head}…{tail}")
}

/// 窗口的来源怎么说（compact.md §2）。
fn source_words(l: Lang, source: WindowSource) -> &'static str {
    match source {
        WindowSource::ModelDefault | WindowSource::Auto => l.pick("the model's own", "模型自己的"),
        WindowSource::Env => l.pick(
            "set by ZCODE_AUTO_COMPACT_WINDOW",
            "由 ZCODE_AUTO_COMPACT_WINDOW 设定",
        ),
        WindowSource::UnknownModel => l.pick("a default for unknown models", "未知模型的默认值"),
    }
}

/// 窗口一行：`200k window, <来源>  ·  compacts at 160k`；放不下时不写来源。
fn window_line(l: Lang, theme: &Theme, usage: &ContextUsage, cols: usize) -> Vec<Span> {
    let w = window_of(usage);
    let tail = if let Some(at) = w.compact_at {
        vec![
            dot(theme),
            sp(l.pick("compacts at ", "压缩于 "), theme.fg3),
            sp(count(at as f64), theme.fg2),
        ]
    } else if !usage.auto_compact {
        vec![
            dot(theme),
            sp(l.pick("autocompact off", "自动压缩已关"), theme.fg3),
        ]
    } else {
        Vec::new()
    };
    let window = l.pick(" window", " 窗口");
    let with_source = |source: bool| {
        let mut line = vec![sp(count(w.max as f64), theme.fg2)];
        line.push(if source {
            sp(
                match l {
                    Lang::En => format!(" window, {}", source_words(l, usage.window_source)),
                    Lang::Zh => format!(" 窗口，{}", source_words(l, usage.window_source)),
                },
                theme.fg3,
            )
        } else {
            sp(window, theme.fg3)
        });
        line.extend(tail.iter().cloned());
        line
    };
    let full = with_source(true);
    let width: usize = full.iter().map(|s| string_width(&s.text)).sum();
    if VALUE_AT + width < cols {
        full
    } else {
        with_source(false)
    }
}

fn mcp_rows(l: Lang, theme: &Theme, cols: usize, servers: &[McpServerInfo]) -> Vec<StyledLine> {
    let at = second_at(cols, SECOND_AT, servers.iter().map(|s| s.name.as_str()));
    servers
        .iter()
        .map(|server| {
            let name = |ink: Style| sp(server.name.clone(), ink);
            match server.state {
                McpServerState::Connected => {
                    let mut second = vec![
                        sp(format!("{:>TOOLS_WIDTH$}", server.tools), theme.fg2),
                        sp(
                            match l {
                                Lang::En if server.tools == 1 => " tool",
                                Lang::En => " tools",
                                Lang::Zh => " 个工具",
                            },
                            theme.fg3,
                        ),
                    ];
                    if !server.machine.is_primary() {
                        second.push(sp(l.pick("   on ", "   在 "), theme.fg3));
                        second.push(sp(server.machine.as_str().to_owned(), theme.fg2));
                    }
                    item(
                        cols,
                        at,
                        sp("✓", Style::fg(theme.ok)),
                        name(Style::DEFAULT),
                        second,
                        None,
                    )
                }
                McpServerState::Pending => item(
                    cols,
                    at,
                    sp("○", theme.fg3),
                    name(Style::DEFAULT),
                    vec![sp(connecting(l), theme.fg3)],
                    None,
                ),
                McpServerState::Failed => item(
                    cols,
                    at,
                    sp("✗", Style::fg(theme.err)),
                    name(Style::DEFAULT),
                    vec![sp(
                        server
                            .reason
                            .clone()
                            .unwrap_or_else(|| failed(l).to_owned()),
                        theme.fg2,
                    )],
                    Some(("/mcp", theme.fg3)),
                ),
                McpServerState::Disabled => item(
                    cols,
                    at,
                    sp("–", theme.fg3),
                    name(theme.fg3),
                    vec![sp(l.pick("turned off", "已关"), theme.fg3)],
                    None,
                ),
                // 要授权（tui-manage.md §6.5）：amber，同设置页的 `needs sign-in`，提示去 `/mcp`。
                McpServerState::NeedsAuth => item(
                    cols,
                    at,
                    sp("!", Style::fg(theme.code)),
                    name(Style::DEFAULT),
                    vec![sp("needs sign-in", Style::fg(theme.code))],
                    Some(("/mcp", theme.fg3)),
                ),
            }
        })
        .collect()
}

fn mcp_tally(l: Lang, theme: &Theme, servers: &[McpServerInfo]) -> Vec<Span> {
    let n = |state: McpServerState| servers.iter().filter(|s| s.state == state).count();
    tally(
        theme,
        &[
            (
                n(McpServerState::Connected),
                l.pick("connected", "已连接"),
                false,
            ),
            (
                n(McpServerState::Pending),
                l.pick("connecting", "连接中"),
                false,
            ),
            (n(McpServerState::Failed), failed(l), true),
        ],
    )
}

fn connecting(l: Lang) -> &'static str {
    l.pick("connecting…", "连接中…")
}

fn failed(l: Lang) -> &'static str {
    l.pick("failed", "失败")
}

fn not_connected(l: Lang) -> &'static str {
    l.pick("not connected", "未连接")
}

fn ready(l: Lang) -> &'static str {
    l.pick("ready", "就绪")
}

fn machine_rows(l: Lang, theme: &Theme, cols: usize, machines: &[MachineRow]) -> Vec<StyledLine> {
    let at = second_at(cols, SECOND_AT, machines.iter().map(|m| m.name.as_str()));
    machines
        .iter()
        .map(|m| {
            let name = sp(m.name.clone(), Style::DEFAULT);
            match &m.status {
                MachineStatus::Ready => {
                    let mut second = vec![sp(ready(l), theme.fg2)];
                    if let Some(ms) = m.latency_ms {
                        second.push(dot(theme));
                        second.push(sp(format!("{ms} ms"), theme.fg3));
                    }
                    if let Some(e) = &m.executor {
                        second.push(dot(theme));
                        second.push(sp(format!("{} {}", e.os, e.arch), theme.fg3));
                    }
                    item(cols, at, sp("✓", Style::fg(theme.ok)), name, second, None)
                }
                MachineStatus::Connecting(_) => item(
                    cols,
                    at,
                    sp("○", theme.fg3),
                    name,
                    vec![sp(connecting(l), theme.fg3)],
                    None,
                ),
                MachineStatus::Reconnecting { .. } => item(
                    cols,
                    at,
                    sp("○", theme.fg3),
                    name,
                    vec![sp(l.pick("reconnecting…", "重连中…"), theme.fg3)],
                    None,
                ),
                MachineStatus::Disconnected => item(
                    cols,
                    at,
                    sp("–", theme.fg3),
                    sp(m.name.clone(), theme.fg3),
                    vec![sp(not_connected(l), theme.fg3)],
                    None,
                ),
                MachineStatus::Failed(failure) => item(
                    cols,
                    at,
                    sp("✗", Style::fg(theme.err)),
                    name,
                    vec![sp(
                        super::i18n_machines::cause(l, &m.name, failure),
                        theme.fg2,
                    )],
                    Some(("/machines", theme.fg3)),
                ),
            }
        })
        .collect()
}

fn machine_tally(l: Lang, theme: &Theme, machines: &[MachineRow]) -> Vec<Span> {
    let n = |f: fn(&MachineStatus) -> bool| machines.iter().filter(|m| f(&m.status)).count();
    tally(
        theme,
        &[
            (n(|s| matches!(s, MachineStatus::Ready)), ready(l), false),
            (
                n(|s| {
                    matches!(
                        s,
                        MachineStatus::Connecting(_) | MachineStatus::Reconnecting { .. }
                    )
                }),
                l.pick("connecting", "连接中"),
                false,
            ),
            (
                n(|s| matches!(s, MachineStatus::Disconnected)),
                not_connected(l),
                false,
            ),
            (
                n(|s| matches!(s, MachineStatus::Failed(_))),
                failed(l),
                true,
            ),
        ],
    )
}

fn hook_rows(theme: &Theme, cols: usize, hooks: &[HookEntry]) -> Vec<StyledLine> {
    let event_width = hooks
        .iter()
        .map(|h| string_width(&h.event))
        .max()
        .unwrap_or(0)
        .saturating_add(EVENT_GAP)
        .min(EVENT_MAX);
    let text_at = NAME_AT + event_width;
    hooks
        .iter()
        .map(|h| {
            let end = cols.saturating_sub(POINTER_MARGIN);
            let source_at = end.saturating_sub(string_width(&h.source));
            let room = source_at.saturating_sub(text_at + POINTER_GAP);
            Row::default()
                .at(
                    NAME_AT,
                    vec![sp(
                        clip(&h.event, event_width.saturating_sub(1)),
                        Style::DEFAULT,
                    )],
                )
                .at(text_at, vec![sp(clip(&h.display, room), theme.fg2)])
                .right(end, vec![sp(h.source.clone(), theme.fg3)])
                .line()
        })
        .collect()
}

impl App {
    /// 要不要问事实：设置显示 Status、看的是主会话（子代理的 Status 不画这些）。
    pub fn session_facts_wanted(&self) -> bool {
        self.viewing.is_none() && self.settings_live() == Some(Live::Status)
    }

    /// 事实到了：替换上一次的（页关上时留着，下次打开先画旧的）。
    pub fn session_facts_arrived(&mut self, facts: SessionFacts) {
        self.session_facts = Some(facts);
    }

    /// ⏎ 复制的 ID：主会话的，查看子代理时是它的子会话的；还没有会话时没有。
    pub(super) fn session_copy_id(&self) -> Option<String> {
        self.stats_session().map(|id| id.as_str().to_owned())
    }

    /// 开始的时刻：恢复的会话是它建的时刻，否则视图第一行的时刻。
    fn session_started_ms(&self) -> Option<u64> {
        self.resumed.as_ref().map(|r| r.created_ms).or_else(|| {
            self.view
                .rows()
                .iter()
                .map(|row| row.created_ms)
                .find(|ms| *ms > 0)
        })
    }

    /// `14:02`，不是今天时 `Nov 1 14:02`。
    fn session_when(&self, at: u64, now_ms: u64) -> String {
        let (at_f, now_f) = (at as f64, now_ms as f64);
        if now_ms.saturating_sub(at) < DAY_MS
            && month_day(at_f, &self.zone) == month_day(now_f, &self.zone)
        {
            clock(at_f, &self.zone)
        } else {
            format!(
                "{} {}",
                month_day(at_f, &self.zone),
                clock(at_f, &self.zone)
            )
        }
    }

    /// Model 行（模型 · 档位、provider）与窗口一行（`window` 是页宽：计数到了、模型有窗口时写；None 不写）。
    fn session_model_rows(
        &self,
        view: &crate::view::Transcript,
        window: Option<usize>,
    ) -> Vec<StyledLine> {
        let theme = &self.ink.theme;
        let l = self.lang;
        let selection = view.selection();
        // 内置家族的名字，否则模型目录里的名字；都没有时不写（自己加的 provider 的 ID 是 UUID）。
        let provider = self
            .session_facts
            .as_ref()
            .and_then(|f| {
                f.families
                    .iter()
                    .find(|f| f.provider == selection.provider)
                    .map(|f| f.name.clone())
                    .or_else(|| {
                        f.providers
                            .iter()
                            .find(|(id, _)| *id == selection.provider)
                            .map(|(_, name)| name.clone())
                    })
            })
            .unwrap_or_default();
        let mut value = vec![sp(selection.model.clone(), Style::DEFAULT)];
        if let Some(level) = selection.reasoning.as_deref().filter(|l| !l.is_empty()) {
            value.push(sp(format!(" · {level}"), theme.fg3));
        }
        if !provider.is_empty() {
            value.push(sp(format!("   {provider}"), theme.fg3));
        }
        let mut out = vec![labeled(theme, l.pick("Model", "模型"), value)];
        if let (Some(cols), Some(usage)) = (window, self.context_now())
            && usage.raw_max_tokens > 0
        {
            out.push(more(window_line(l, theme, usage, cols)));
        }
        out
    }

    /// Mode 行：权限模式的名字，plan 开时前面 `Plan mode  ·  `。
    fn session_mode_row(&self, view: &crate::view::Transcript) -> StyledLine {
        let theme = &self.ink.theme;
        let l = self.lang;
        let execution = view.execution();
        let mut value = Vec::new();
        if execution.plan {
            value.extend([sp(l.pick("Plan mode", "计划模式"), theme.fg2), dot(theme)]);
        }
        value.push(sp(
            super::i18n::mode_label(l, execution.mode),
            Style::DEFAULT,
        ));
        labeled(theme, l.pick("Mode", "模式"), value)
    }

    /// 查看子代理时：它自己的会话、模型与模式，说 Context 只为主会话计数（§4.5）。
    fn viewed_session_rows(&self, viewing: &super::agents::Viewing) -> Vec<StyledLine> {
        let theme = &self.ink.theme;
        let l = self.lang;
        let status = match viewing.status {
            SubagentStatus::Running => sp(l.pick("running", "运行中"), Style::fg(theme.work)),
            SubagentStatus::Completed => sp(l.pick("completed", "已完成"), theme.fg3),
            SubagentStatus::Failed => sp(failed(l), theme.fg3),
            SubagentStatus::Killed => sp(l.pick("killed", "已停止"), theme.fg3),
        };
        let mut out = vec![labeled(
            theme,
            l.pick("Subagent", "子代理"),
            vec![
                sp(viewing.type_name().to_owned(), Style::DEFAULT),
                dot(theme),
                status,
            ],
        )];
        if let Some(id) = self.session_copy_id() {
            out.push(more(vec![sp(id, theme.fg2)]));
        }
        let parent = self.view.title();
        if !parent.is_empty() {
            out.push(more(vec![
                sp(l.pick("started by ", "启动它的是 "), theme.fg3),
                sp(parent.to_owned(), theme.fg2),
            ]));
        }
        out.extend(self.session_model_rows(&viewing.view, None));
        out.push(self.session_mode_row(&viewing.view));
        out.push(StyledLine::EMPTY);
        out.push(
            Row::default()
                .at(
                    LABEL_AT,
                    vec![
                        sp(l.pick("Context", "上下文"), theme.fg2),
                        sp(
                            l.pick(
                                " is counted for the main session only.",
                                " 只为主会话计数。",
                            ),
                            theme.fg3,
                        ),
                    ],
                )
                .line(),
        );
        out
    }

    /// Status 段的行（§4.5）。
    pub(super) fn session_lines(&self, width: usize, now_ms: u64) -> Vec<StyledLine> {
        let theme = &self.ink.theme;
        let l = self.lang;
        let session = l.pick("Session", "会话");
        let cols = width.clamp(1, MEASURE_MAX + MEASURE_MARGIN);
        let mut lines = Vec::new();
        if let Some(viewing) = &self.viewing {
            lines.extend(self.viewed_session_rows(viewing));
            return lines;
        }
        let facts = self.session_facts.as_ref();
        // Session：标题、ID、开始的时刻与回合、工作时长。
        match self.session.as_deref() {
            Some(id) => {
                let title = self.view.title();
                let title = if title.is_empty() {
                    sp(l.pick("Untitled", "无标题"), theme.fg3)
                } else {
                    sp(
                        clip(title, cols.saturating_sub(VALUE_AT + 1)),
                        Style::DEFAULT,
                    )
                };
                lines.push(labeled(theme, session, vec![title]));
                lines.push(more(vec![sp(id_text(id, cols), theme.fg2)]));
                let (turns, active_ms) = self.view.turn_totals();
                let mut third = Vec::new();
                if let Some(at) = self.session_started_ms() {
                    third.extend([
                        sp(l.pick("started ", "开始于 "), theme.fg3),
                        sp(self.session_when(at, now_ms), theme.fg2),
                    ]);
                }
                if turns > 0 {
                    if !third.is_empty() {
                        third.push(dot(theme));
                    }
                    third.extend([
                        sp(turns.to_string(), theme.fg2),
                        sp(
                            match l {
                                Lang::En if turns == 1 => " turn",
                                Lang::En => " turns",
                                Lang::Zh => " 个回合",
                            },
                            theme.fg3,
                        ),
                        dot(theme),
                        sp(stats::working(active_ms as f64), theme.fg2),
                        sp(l.pick(" working", " 工作"), theme.fg3),
                    ]);
                }
                if !third.is_empty() {
                    lines.push(more(third));
                }
            }
            None => lines.push(labeled(
                theme,
                session,
                vec![
                    sp(l.pick("not started", "还没开始"), theme.fg3),
                    dot(theme),
                    sp(
                        l.pick(
                            "your first message starts one",
                            "你的第一条消息开始一个会话",
                        ),
                        theme.fg3,
                    ),
                ],
            )),
        }
        lines.extend(self.session_model_rows(&self.view, Some(cols)));
        // Account：provider 是内置家族时才有。
        let provider = &self.view.selection().provider;
        if let Some(family) =
            facts.and_then(|f| f.families.iter().find(|f| &f.provider == provider))
        {
            let value = if family.signed_in {
                let mut v = Vec::new();
                if !family.account.is_empty() {
                    v.extend([sp(family.account.clone(), Style::DEFAULT), dot(theme)]);
                }
                v.push(sp(super::settings::sections::plan_name(family), theme.fg2));
                v
            } else {
                vec![sp(l.pick("not signed in", "未登录"), theme.fg3)]
            };
            lines.push(labeled(theme, l.pick("Account", "账号"), value));
        }
        lines.push(self.session_mode_row(&self.view));
        let mut directory = vec![sp(
            clip(&self.workspace_label(), cols.saturating_sub(VALUE_AT + 1)),
            Style::DEFAULT,
        )];
        if let Some(branch) = facts.and_then(|f| f.branch.as_deref()) {
            directory.extend([dot(theme), sp(branch.to_owned(), theme.fg3)]);
        }
        lines.push(labeled(theme, l.pick("Directory", "目录"), directory));
        let mut version = vec![sp(format!("zcode {}", self.version), Style::DEFAULT)];
        if let Some(newer) = self.newer_release.as_deref().filter(|_| cols >= MEDIUM) {
            version.extend([
                dot(theme),
                sp(newer.to_owned(), theme.fg2),
                sp(l.pick(" is out, run ", " 已发布，运行 "), theme.fg3),
                sp("zcode update", theme.fg2),
            ]);
        }
        lines.push(labeled(theme, l.pick("Version", "版本"), version));
        // MCP servers：事实到了才知道。
        lines.push(StyledLine::EMPTY);
        let servers = l.pick("MCP servers", "MCP 服务器");
        match facts {
            None => {
                lines.push(headed(theme, cols, servers, Vec::new()));
                lines.push(
                    Row::default()
                        .at(
                            MARK_AT,
                            vec![sp(
                                l.pick("Checking the servers…", "正在检查服务器…"),
                                theme.fg3,
                            )],
                        )
                        .line(),
                );
            }
            Some(f) if f.mcp.is_empty() => {
                lines.push(headed(theme, cols, servers, Vec::new()));
                lines.push(
                    Row::default()
                        .at(
                            MARK_AT,
                            vec![sp(l.pick("No MCP servers", "没有 MCP 服务器"), theme.fg3)],
                        )
                        .line(),
                );
            }
            Some(f) => {
                lines.push(headed(theme, cols, servers, mcp_tally(l, theme, &f.mcp)));
                lines.extend(mcp_rows(l, theme, cols, &f.mcp));
            }
        }
        if self.machines_enabled && !self.machines.is_empty() {
            lines.push(StyledLine::EMPTY);
            lines.push(headed(
                theme,
                cols,
                l.pick("Machines", "机器"),
                machine_tally(l, theme, &self.machines),
            ));
            lines.extend(machine_rows(l, theme, cols, &self.machines));
        }
        if !self.memory_files.is_empty() {
            lines.push(StyledLine::EMPTY);
            let n = plural(l, self.memory_files.len(), "file", "files", "个文件");
            lines.push(headed(
                theme,
                cols,
                l.pick("Memory files", "规则文件"),
                vec![sp(n, theme.fg3)],
            ));
            // 种类对齐在最长的路径之后 3 列，至多到行宽的一半（更长的路径截断）。
            let longest = self
                .memory_files
                .iter()
                .map(|f| string_width(&f.display))
                .max()
                .unwrap_or(0);
            let kind_at = (NAME_AT + longest + KIND_GAP).min(cols / 2);
            for file in &self.memory_files {
                let path = clip(&file.display, kind_at.saturating_sub(NAME_AT + KIND_GAP));
                lines.push(
                    Row::default()
                        .at(NAME_AT, vec![sp(path, Style::DEFAULT)])
                        .at(
                            kind_at,
                            vec![sp(
                                super::settings::data::scope_word(l, &file.label),
                                theme.fg3,
                            )],
                        )
                        .line(),
                );
            }
        }
        if let Some(hooks) = facts.map(|f| &f.hooks).filter(|h| !h.is_empty()) {
            lines.push(StyledLine::EMPTY);
            let n = plural(l, hooks.len(), "hook", "hooks", "个钩子");
            lines.push(headed(
                theme,
                cols,
                l.pick("Hooks", "钩子"),
                vec![sp(n, theme.fg3)],
            ));
            lines.extend(hook_rows(theme, cols, hooks));
        }
        lines
    }
}

#[cfg(test)]
#[path = "session_page_tests.rs"]
mod tests;
