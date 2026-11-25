//! 页（docs/specs/rust-cli/tui-kit.md §4.0「页」；Puffin `frame_of` 的设置页与 `/usage` 详情页）：正文换成一页，底部栈
//! （提示、composer_gap、板、状态栏）留着；没有坞、没有层。页的行从它的顶行起填满底部栈上面，看不见的行在第一或最后一行写
//! `↑ n more` / `↓ n more`（kit `frame::page`）。页的按键提示写在状态栏第 1 行的右边，输入行的槽空着（tui-kit.md §4.0
//! 「页的键」，2026-11-12 统一：以前这里的页写在槽里、设置页写在状态栏，两处不一样）。
//!
//! 以前的独立页面、详细视图与 `/usage` 占满整屏（`Overlay::full_screen`）：自己的标题行、最后一行的提示，没有输入框与
//! 状态栏。页的主人把它们搬到这里时在 [`body`] 加一个分支，并把它从 `Overlay::full_screen` 拿掉；信任询问照 Puffin 仍占满
//! 整屏。
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::app::App;

/// 一页的内容：排好的行（按屏宽）、顶行（滚动的位置，画的时候夹到最后一屏）、这一页的键、它拿不拿键（拿键时草稿行没有光标）。
#[derive(Debug, Clone, Default)]
pub struct PageBody {
    pub lines: Vec<StyledLine>,
    pub top: usize,
    /// 这一页的键（kit `key_hints::spans`，最后一对是 esc 与它回到哪里）：画在状态栏第 1 行的右边（`pane::bottom`）。
    pub keys: Vec<Span>,
    pub owns_keys: bool,
    /// 内容要按底部栈之上正好的高度排（驾驶舱，`draw.rs` 算好高度之后问它）：`lines` 空着。
    pub fit: bool,
}

/// 页的键里“回去”的那一对：`esc back`（中文 `esc 返回`）。
pub fn esc_back(lang: super::i18n::Lang) -> (String, String) {
    let word = match lang {
        super::i18n::Lang::En => "back",
        super::i18n::Lang::Zh => "返回",
    };
    ("esc".to_owned(), word.to_owned())
}

/// 开着的页（按 `app.overlay`）；其余以前的全屏视图照旧走 `draw.rs` 的 `full_screen_frame`。`now_ms` 是墙上时间。
pub fn body(app: &App, width: usize, now: std::time::Instant, now_ms: u64) -> Option<PageBody> {
    // `/usage` 的详情页（usage_plans.rs）。
    let mut page = app
        .usage_page_body(width, super::status_motion::tick_at(app, now), now_ms)
        // 详情页里的网址（服务给的说明、错误）也能点（tui-select.md §10「页里的网址」）。
        .map(|mut page| {
            super::links::mark(&mut page.lines);
            page
        })
        // `/resume` 列表上 ⌃t 读的会话（sessions_steps.rs）。
        .or_else(|| app.sessions_page_body(width, now_ms))
        // ← 的驾驶舱（cockpit_view.rs）。
        .or_else(|| app.cockpit_page_body())?;
    // 刚按 `c` 复制了这一页：键的位置说一句，到下一个键（page_copy.rs）。
    if app.page_copied.is_some() {
        page.keys = super::page_copy::said(app);
    }
    Some(page)
}
