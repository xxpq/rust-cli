//! 设置的三段 Status、Context（与它的 Details）、Stats（docs/specs/rust-cli/tui-panels.md §4，用户决定 2026-11-12）：只读的页，
//! 内容是排好的行（kit `settings::Row::Lines`），不经设置的 `Sources::read`。数据各有唯一的来源：Status 读 `App` 已有的与
//! `App.session_facts`，Context 与 Details 读 `App.context_count`，Stats 读 `Settings.stats`；它们显示时设置的 `shown` 发出要问的
//! （`mod.rs`），这里只画与处理键。
//!
//! 以前三样是板上的数字卡（`/stats`，别名 `/context`、`/status`）与它打开的统计的页（Context 与 Session 两个标签）以及 `/usage all`
//! 的「This computer」页；内容不变，只换了地方（Stats 改成热力图与概览的样子）。
use rust_cli_tui_kit::app::widgets::settings::view::{lead_rows, pane_width};
use rust_cli_tui_kit::app::widgets::usage::view::LEAD;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::Settings;
use super::keys::Pressed;
use super::page::{Page, Row, View};
use crate::ui::app::{Action, App};
use crate::ui::context_tree::{self, Move};
use crate::ui::session_page::Copied;

/// 只读的页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    Status,
    Context,
    /// Context › Details：大纲、余量与建议（§4.3）。
    Details,
    Stats,
}

impl Live {
    pub fn of(page: &Page) -> Option<Live> {
        match page {
            Page::Top("status") => Some(Live::Status),
            Page::Top("context") => Some(Live::Context),
            Page::ContextDetails => Some(Live::Details),
            Page::Top("stats") => Some(Live::Stats),
            _ => None,
        }
    }
}

/// Details 的面包屑（段名之后的一级），按界面语言。
pub fn details(lang: crate::ui::i18n::Lang) -> &'static str {
    lang.pick("Details", "详情")
}
/// 选中的行离一屏的上下边至少这么多行（边上那一行可能是 `↑ n more`）。
const MARGIN: usize = 1;

/// 页框的左边距（kit `usage::view::LEAD`，3 列）：Status、Context、Stats 的行照以前的页从第 3 列起排，设置的窗格自己有左边，
/// 排好之后去掉。行首不是空格时到那里为止（没有边距的行原样留着）。
fn unlead(lines: Vec<StyledLine>) -> Vec<StyledLine> {
    lines
        .into_iter()
        .map(|line| {
            let mut left = LEAD.len();
            let spans = line
                .spans
                .into_iter()
                .filter_map(|span| {
                    if left == 0 {
                        return Some(span);
                    }
                    let text = span.text.as_ref();
                    let cut = text.bytes().take(left).take_while(|b| *b == b' ').count();
                    left = if cut == text.len() { left - cut } else { 0 };
                    let rest = text[cut..].to_owned();
                    (!rest.is_empty()).then(|| Span {
                        text: rest.into(),
                        ..span
                    })
                })
                .collect();
            StyledLine { spans, ..line }
        })
        .collect()
}

/// 让第 `at` 行看得见的顶行：离一屏（`rows` 行）的上下边至少 [`MARGIN`] 行，夹到最后一屏。
fn follow(top: usize, at: usize, rows: usize, total: usize) -> usize {
    let max_top = total.saturating_sub(rows);
    let top = top.min(max_top);
    let top = if at < top + MARGIN {
        at.saturating_sub(MARGIN)
    } else if rows > 2 * MARGIN && at + MARGIN + 1 > top + rows {
        at + MARGIN + 1 - rows
    } else {
        top
    };
    top.min(max_top)
}

impl Settings {
    /// 现在显示的页的顶行：记下的是别的页时从头。
    pub fn top(&self) -> usize {
        match &self.scroll {
            Some((page, top)) if *page == self.page() => *top,
            _ => 0,
        }
    }

    fn set_top(&mut self, top: usize) {
        self.scroll = Some((self.page(), top));
    }
}

impl App {
    /// 设置显示着的只读页（设置开着、没有在搜索）。
    pub fn settings_live(&self) -> Option<Live> {
        let ui = self.settings.as_ref()?;
        if ui.searching() {
            return None;
        }
        Live::of(&ui.page())
    }

    /// 只读页的行，按窗格的宽（kit `pane_width`）排好；Details 另给选中的行在第几行。
    fn live_lines(
        &self,
        ui: &Settings,
        live: Live,
        now_ms: u64,
    ) -> (Vec<StyledLine>, Option<usize>) {
        // 内容照以前的页带 3 列的左边距排，宽要多给这 3 列，去掉之后正好是窗格的宽。
        let width = pane_width(ui.width) + LEAD.len();
        match live {
            Live::Status => (unlead(self.session_lines(width, now_ms)), None),
            Live::Context if self.viewing.is_some() => (unlead(vec![main_only(self)]), None),
            Live::Context => (unlead(self.context_overview(width, now_ms)), None),
            Live::Details => {
                let page = self.context_details(&ui.context, width, now_ms);
                (unlead(page.lines), page.chosen)
            }
            Live::Stats => (
                unlead(super::super::stats_local::lines(
                    self.lang,
                    &self.ink.theme,
                    ui.stats.as_ref(),
                    width,
                )),
                None,
            ),
        }
    }

    /// 只读页的样子：面包屑（Details 多一级）、段的说明（Details 没有）与排好的行。
    pub(super) fn live_view(&self, ui: &Settings, live: Live, now_ms: u64) -> View {
        let (lines, _) = self.live_lines(ui, live, now_ms);
        let (crumbs, desc) = match live {
            Live::Details => (vec![details(self.lang).to_owned()], ""),
            _ => (
                Vec::new(),
                crate::ui::i18n_settings::words(self.lang)
                    .section(super::page::section_of(&ui.page()))
                    .desc,
            ),
        };
        View::new(crumbs, desc, vec![Row::Lines(lines)])
    }

    /// 最后一屏的顶行：说明的行加内容的行，减去上一帧窗格能放的行。
    fn live_max_top(&self, ui: &Settings, live: Live) -> usize {
        let view = self.live_view(ui, live, 0);
        let lines = match view.rows.first() {
            Some(Row::Lines(lines)) => lines.len(),
            _ => 0,
        };
        (lead_rows(&view.desc, pane_width(ui.width)) + lines).saturating_sub(ui.pane_rows)
    }

    /// 状态栏右边的键（§4.1）：同别的页的写法，`⏎` 与 `⇥` 是这一页自己的。
    pub(super) fn live_keys(
        &self,
        ui: &Settings,
        live: Live,
        narrow: bool,
        asking: bool,
    ) -> Vec<(String, String)> {
        // 说明按界面语言（tui-kit.md §4.5「语言」）。
        let said = crate::ui::i18n_settings::words(self.lang);
        let pair = |k: &str, w: &str| (k.to_owned(), said.key_word(w).to_owned());
        let mut out = Vec::new();
        if live == Live::Details {
            out.push(pair("↑↓", "choose"));
            let open = self.context_outline().and_then(|nodes| {
                let rows = context_tree::visible(&nodes, &ui.context.open);
                let at = context_tree::chosen_at(&rows, &ui.context.chosen);
                rows.get(at).and_then(|(_, shown)| shown.open)
            });
            match open {
                Some(false) => out.push(pair("⏎", "expand")),
                Some(true) => out.push(pair("⏎", "collapse")),
                None => {}
            }
        } else {
            out.push(pair("↑↓", "scroll"));
            out.push(if narrow {
                pair("←→", "section")
            } else {
                pair("←", "sections")
            });
            match live {
                Live::Status if self.session_copy_id().is_some() => out.push(pair("⏎", "copy id")),
                Live::Context if self.viewing.is_none() => out.push(pair("⏎", "details")),
                Live::Stats => out.push(pair("⇥", "range")),
                _ => {}
            }
        }
        let esc = if asking {
            "answer it"
        } else if live == Live::Details {
            "back"
        } else {
            "close"
        };
        out.push(pair("esc", esc));
        out
    }

    /// 只读页上的键（§4.1、§4.3、§4.5、§4.6）：外层 None 是不归这里，交给设置的通用处理（← 回目录、esc、打字搜索…）。
    pub(super) fn live_key(
        &mut self,
        ui: &Settings,
        live: Live,
        key: &Pressed,
    ) -> Option<Option<Action>> {
        if live == Live::Details {
            return self.details_key(ui, key);
        }
        let screen = ui.pane_rows.saturating_sub(1).max(1) as isize;
        let by: isize = match key {
            Pressed::Up => -1,
            Pressed::Down => 1,
            Pressed::PageUp => -screen,
            Pressed::PageDown => screen,
            Pressed::Home => isize::MIN / 2,
            Pressed::End => isize::MAX / 2,
            Pressed::Enter => return self.live_enter(live),
            Pressed::Tab if live == Live::Stats => {
                let action = self.stats_next_range();
                return Some(action);
            }
            _ => return None,
        };
        self.live_scroll(by);
        Some(None)
    }

    /// 滚一页的顶行，夹到最后一屏。
    pub fn live_scroll(&mut self, by: isize) {
        let (Some(ui), Some(live)) = (self.settings.clone(), self.settings_live()) else {
            return;
        };
        let max_top = self.live_max_top(&ui, live);
        if let Some(u) = self.settings.as_mut() {
            let top = u.top().min(max_top).saturating_add_signed(by).min(max_top);
            u.set_top(top);
        }
    }

    /// ⏎：Status 复制会话 ID（状态栏右边说一句，到下一个键）；Context 打开 Details（查看子代理时没有）。
    fn live_enter(&mut self, live: Live) -> Option<Option<Action>> {
        match live {
            Live::Status => {
                let id = self.session_copy_id();
                if id.is_some() {
                    self.page_copied = Some(Copied::SessionId);
                }
                Some(id.map(Action::Copy))
            }
            Live::Context if self.viewing.is_none() => {
                self.settings_details();
                Some(None)
            }
            _ => Some(None),
        }
    }

    /// 打开 Details（Context 上 ⏎、`/context all`）：开合与选中留着上一次的（设置开着的期间）。计数已经在问，不再问。
    pub(in crate::ui) fn settings_details(&mut self) {
        if let Some(ui) = self.settings.as_mut()
            && ui.page() != Page::ContextDetails
        {
            ui.stack.push(Page::ContextDetails);
            ui.said = None;
        }
    }

    /// Details 的键（§4.3）：↑↓、PgUp/PgDn、Home/End 移动选中，⏎ 开合，→ 打开，← 合上或去上一层；页跟着选中滚。esc 与打字交给
    /// 设置（esc 回到 Context）。
    fn details_key(&mut self, ui: &Settings, key: &Pressed) -> Option<Option<Action>> {
        let screen = ui.pane_rows.saturating_sub(1).max(1) as isize;
        let how = match key {
            Pressed::Up => Move::By(-1),
            Pressed::Down => Move::By(1),
            Pressed::PageUp => Move::By(-screen),
            Pressed::PageDown => Move::By(screen),
            Pressed::Home => Move::First,
            Pressed::End => Move::Last,
            Pressed::Enter => Move::Toggle,
            Pressed::Right => Move::Open,
            Pressed::Left => Move::Close,
            _ => return None,
        };
        let Some(nodes) = self.context_outline() else {
            return Some(None);
        };
        let mut next = ui.clone();
        context_tree::step(
            &nodes,
            &mut next.context.open,
            &mut next.context.chosen,
            how,
        );
        let (lines, chosen) = self.live_lines(&next, Live::Details, 0);
        let top = chosen.map_or(next.top(), |at| {
            follow(next.top(), at, ui.pane_rows, lines.len())
        });
        if let Some(u) = self.settings.as_mut() {
            u.context = next.context;
            u.set_top(top);
        }
        Some(None)
    }

    /// 只读页的数在淡入（Context 与 Details 的计数变了，`context_fading`）。
    pub(super) fn live_moving(&self, now: std::time::Instant) -> bool {
        matches!(self.settings_live(), Some(Live::Context | Live::Details))
            && self.context_fading(now)
    }
}

/// 查看子代理时的 Context：计数只为主会话（§4.1）。
fn main_only(app: &App) -> StyledLine {
    let theme = &app.ink.theme;
    StyledLine::new(vec![
        Span::plain(LEAD),
        Span::new(app.lang.pick("Context", "上下文"), theme.fg2),
        Span::new(
            app.lang.pick(
                " is counted for the main session only.",
                " 只为主会话计数。",
            ),
            theme.fg3,
        ),
    ])
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
