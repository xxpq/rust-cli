//! 复制回答里的公式与 Mermaid 图（docs/specs/rust-cli/tui-kit.md §3.5「复制」，用户决定 2026-11-12）：拖选复制与 `/export`
//! 给原文（TeX 与 mermaid 源码，连同定界符与围栏），不给屏幕上的方框图与 Unicode 公式；只选了一部分时给整块的原文；
//! 画不出的图的原因不进复制的字。修改前复制出的是屏幕上的样子。
use crate::app::select_units::{copied_rows, copy_text};
use crate::render::pool::StylePool;
use crate::render::{Decor, Style, StyledLine};

use super::{doc, laid, md, rich_theme};
use crate::markdown::render::render;

const FORMULA: &str = "$$\nx=\\frac{1}{2}\n$$";
const DIAGRAM: &str = "```mermaid\nflowchart LR\n    A[Request] --> B[Reply]\n```";

fn plain_laid(src: &str) -> Vec<StyledLine> {
    laid(&md(src), 60)
}

fn rich_laid(src: &str) -> Vec<StyledLine> {
    laid(&render(&doc(src), &rich_theme()), 60)
}

/// 全选复制出的字。
fn all(ls: &[StyledLine]) -> String {
    copy_text(ls, 0, usize::MAX - 1)
}

/// 第一个含 `needle` 的行。
fn row_of(ls: &[StyledLine], needle: &str) -> usize {
    ls.iter()
        .position(|l| l.plain().contains(needle))
        .unwrap_or_else(|| {
            panic!(
                "{needle}: {:?}",
                ls.iter().map(StyledLine::plain).collect::<Vec<_>>()
            )
        })
}

#[test]
fn a_display_formula_copies_as_its_tex_source() {
    let src = format!("intro\n\n{FORMULA}\n\nafter");
    for ls in [plain_laid(&src), rich_laid(&src)] {
        assert_eq!(all(&ls), format!("intro\n\n{FORMULA}\n\nafter"));
    }
}

#[test]
fn part_of_a_formula_copies_the_whole_source() {
    let ls = plain_laid(&format!("intro\n\n{FORMULA}\n\nafter"));
    // 只拖过 `x=─` 那一行的两格。
    let r = row_of(&ls, "x=─");
    assert_eq!(copy_text(&ls[r..=r], 3, 4), FORMULA);
    // 从公式中间拖到下面的段落。
    let end = row_of(&ls, "after");
    assert_eq!(copy_text(&ls[r..=end], 4, 7), format!("{FORMULA}\n\nafter"));
}

#[test]
fn a_mermaid_diagram_copies_as_its_fenced_source() {
    let src = format!("intro\n\n{DIAGRAM}\n\nafter");
    let ls = plain_laid(&src);
    assert!(ls.iter().any(|l| l.plain().contains('┌')));
    assert_eq!(all(&ls), format!("intro\n\n{DIAGRAM}\n\nafter"));
    // rich：面板的上下边代替空行，复制时同别的代码块没有空行。
    let ls = rich_laid(&src);
    assert!(ls.iter().any(|l| l.plain().contains('┌')));
    assert_eq!(all(&ls), format!("intro\n{DIAGRAM}\nafter"));
    // 只选了图里的一个框。
    let ls = plain_laid(DIAGRAM);
    let r = row_of(&ls, "│ Request │");
    assert_eq!(copy_text(&ls[r..=r], 6, 12), DIAGRAM);
}

#[test]
fn a_diagram_that_cannot_be_drawn_copies_its_source_without_the_reason() {
    let src = "```mermaid\npie\n\"Cats\": 2\n```";
    for ls in [plain_laid(src), rich_laid(src)] {
        assert!(ls[0].plain().contains("Mermaid diagram"));
        assert_eq!(all(&ls), src);
    }
}

#[test]
fn a_formula_in_a_quote_keeps_the_quote_rail() {
    let ls = plain_laid("> $$\n> x^2\n> $$");
    assert!(ls.iter().any(|l| l.plain().contains("x²")));
    assert_eq!(all(&ls), "│ $$\n│ x^2\n│ $$");
}

#[test]
fn an_inline_formula_copies_as_its_source() {
    let ls = plain_laid("a $\\alpha^2$ and \\(\\beta\\) b");
    assert_eq!(ls[0].plain(), "   a α² and β b");
    assert_eq!(all(&ls), "a $\\alpha^2$ and \\(\\beta\\) b");
    // 只选了 `α²` 的第一格。
    assert_eq!(copy_text(&ls, 5, 5), "$\\alpha^2$");
}

#[test]
fn the_export_rows_write_the_sources() {
    let ls = plain_laid(&format!("intro\n\n{FORMULA}\n\n{DIAGRAM}"));
    let rows = copied_rows(&ls);
    assert_eq!(
        rows,
        [
            "   intro",
            "",
            "   $$",
            "   x=\\frac{1}{2}",
            "   $$",
            "",
            "   ```mermaid",
            "   flowchart LR",
            "       A[Request] --> B[Reply]",
            "   ```",
        ]
    );
}

#[test]
fn the_original_is_not_part_of_how_a_span_looks() {
    // 渲染器按样子合并：带原文的段仍算没有装饰（样式池不存它）。
    let tagged = Decor {
        copy: Some("$x$".into()),
        ..Decor::PLAIN
    };
    assert!(tagged.is_plain());
    let mut pool = StylePool::new();
    let plain = pool.intern(Style::DEFAULT, &Decor::PLAIN);
    assert_eq!(pool.intern(Style::DEFAULT, &tagged), plain);
    let link = Decor::link("https://a.example");
    let linked = pool.intern(Style::UNDERLINE, &link);
    let both = Decor {
        copy: Some("$x$".into()),
        ..link
    };
    assert_eq!(pool.intern(Style::UNDERLINE, &both), linked);
    assert!(
        pool.decor_of(linked).copy.is_none(),
        "the pool keeps no source"
    );
    let ls = rich_laid(FORMULA);
    assert!(
        ls.iter()
            .flat_map(|l| l.spans.iter())
            .any(|sp| sp.decor.copy.is_some()),
        "the formula's spans carry the source"
    );
}
