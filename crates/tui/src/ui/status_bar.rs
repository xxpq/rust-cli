//! 状态栏（docs/specs/rust-cli/tui-display.md §2.6；tui-kit.md §4.2「footer」一行）：左边说谁——权限模式与 line 墨色的
//! `⇧⇥`、目标、就绪的远端机器；右边说多少与用什么——速度、缓存命中、上下文占用、上下文快满的提示、模型与档位。总是一行。
//! 按键不在这里，在输入行的槽里（`input_box.rs`、`footer.rs` 的 [`super::footer::slot`]）。
//!
//! 排版、部分与数字的写法都是 kit 的（`widgets::status_bar` 的 `line` / `fit` / `left` / `model_parts`，`widgets::stats` 的
//! `parts`）；这里只把旧界面的数据交给它们，模式的名字与两个百分数的名字按语言写（kit 只有英文）。
use rust_cli_tui_kit::app::widgets::stats;
use rust_cli_tui_kit::app::widgets::status_bar::{self as kit, Left};
use rust_cli_tui_kit::render::{Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

pub use rust_cli_tui_kit::app::widgets::stats::{Figures, Window};
pub use rust_cli_tui_kit::app::widgets::status_bar::{Part, key_parts};

/// 模式的部分与挨着它的 `⇧⇥`（kit `left` 的 `mode` 与 `tab`，名字按语言写）。`mode` 是（文字，样式）的几段，模式变了之后
/// `age` 个 tick，从底色淡入（kit `fade::parts`，Puffin `mode_at`；ages 见 status_motion.rs）。`⇧⇥` 总是写：查看子代理时
/// 它也切换主会话的模式（2026-11-12，tui-panels.md §1.7；以前查看时不写）。
pub fn mode_parts(theme: &Theme, mode: Vec<(String, Style)>, age: i64) -> Vec<Part> {
    let spans = rust_cli_tui_kit::app::widgets::fade::parts(theme, mode, age, None);
    vec![Part::new("mode", spans, 100), kit::tab_part(theme)]
}

/// 模式后面目标的记号与字（kit `left` 的 `goal` 与 `goalWords`；记号与字是 [`super::app::App::goal_status_parts`]）；目标出现
/// 或换了状态之后 `age` 个 tick（Puffin `goal_at`）。
pub fn goal_parts(theme: &Theme, goal: Option<(kit::Inked, kit::Inked)>, age: i64) -> Vec<Part> {
    let Some((mark, words)) = goal else {
        return Vec::new();
    };
    kit::left(
        theme,
        &Left {
            goal: Some((mark, words, age)),
            ..Left::default()
        },
    )
}

/// 就绪的远端机器（`@dev-a +1`，kit `machine_rows::where_`）：左边最后、窄时最先走。
pub fn machine_parts(theme: &Theme, machines: Option<&str>) -> Vec<Part> {
    kit::left(
        theme,
        &Left {
            machines,
            ..Left::default()
        },
    )
}

/// shell 模式的左边（kit `shell_left`）：进模式之后 `age` 个 tick，同 `!` 一起淡入（Puffin `shell_at`）。
pub fn shell_left(theme: &Theme, age: i64) -> Vec<Part> {
    kit::shell_left(theme, age)
}

/// 右边「多少」的部分：速度、缓存命中、上下文占用（kit `stats::parts`）。
pub fn figures(theme: &Theme, figures: &Figures<'_>) -> Vec<Part> {
    stats::parts(theme, figures)
}

/// 右端的模型与档位（kit `model_parts`）：`/model` 选了之后 `age` 个 tick 淡入（Puffin `label_at`）。
pub fn model_parts(theme: &Theme, model: &str, effort: &str, age: i64) -> Vec<Part> {
    kit::model_parts(theme, model, effort, age)
}

/// 排好的状态栏：一行（kit `line`）。
pub fn lines(
    theme: &Theme,
    left: &[Part],
    group: &[Part],
    note: &[Part],
    width: usize,
) -> Vec<StyledLine> {
    vec![StyledLine::new(kit::line(
        left, group, note, width, theme.line,
    ))]
}

/// 两个百分数的名字（tui-display.md §2.6：缓存命中与上下文占用都是百分数，要标记）。
pub fn figure_words(lang: super::i18n::Lang) -> (&'static str, &'static str) {
    match lang {
        super::i18n::Lang::En => ("cache", "ctx"),
        super::i18n::Lang::Zh => ("缓存", "上下文"),
    }
}

#[cfg(test)]
#[path = "status_bar_tests.rs"]
mod tests;
