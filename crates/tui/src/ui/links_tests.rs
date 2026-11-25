//! 别处的网址（docs/specs/rust-cli/tui-select.md §10「别处的网址也能点」）：认出的网址那几格带 `Decor.link`。
use rust_cli_tui_kit::render::{Continuation, Decor, Span, Style, StyledLine};

use super::mark;

const URL: &str = "https://example.com/ci/run/42";

fn line(parts: &[&str]) -> StyledLine {
    StyledLine::new(parts.iter().map(|p| Span::plain((*p).to_owned())).collect())
}

/// 每一段的（文字，链接）。
fn pieces(line: &StyledLine) -> Vec<(String, String)> {
    line.spans
        .iter()
        .map(|s| (s.text.to_string(), s.decor.link.clone()))
        .collect()
}

#[test]
fn a_url_in_plain_text_gets_its_own_linked_span() {
    let style = Style::fg(rust_cli_tui_kit::render::Color::Indexed(3));
    let mut lines = vec![StyledLine::new(vec![
        Span::plain("  ⎿  "),
        Span::new(format!("CI: \"{URL}\", done"), style),
    ])];
    mark(&mut lines);
    assert_eq!(
        pieces(&lines[0]),
        [
            ("  ⎿  ".into(), String::new()),
            ("CI: \"".into(), String::new()),
            (URL.into(), URL.into()),
            ("\", done".into(), String::new()),
        ]
    );
    // 样式不变：只多了链接。
    assert!(lines[0].spans[1..].iter().all(|s| s.style == style));
    assert_eq!(lines[0].plain(), format!("  ⎿  CI: \"{URL}\", done"));
}

/// 软折行：几行接起来认，跨行的网址每一段都带整条网址；续行前面的缩进不算；折行处丢了空格时网址到那里为止。
#[test]
fn a_soft_wrapped_url_links_every_piece_to_the_whole_url() {
    let cont = |at, space| Some(Continuation { at, space });
    let mut lines = vec![
        line(&["   see https://example.com/ci"]),
        StyledLine {
            cont: cont(3, false),
            ..line(&["   /run/42 then"])
        },
        StyledLine {
            cont: cont(3, true),
            ..line(&["   https://e.com/x"])
        },
    ];
    mark(&mut lines);
    assert_eq!(
        pieces(&lines[0]),
        [
            ("   see ".into(), String::new()),
            ("https://example.com/ci".into(), URL.into())
        ]
    );
    assert_eq!(
        pieces(&lines[1]),
        [
            ("   ".into(), String::new()),
            ("/run/42".into(), URL.into()),
            (" then".into(), String::new())
        ]
    );
    assert_eq!(
        pieces(&lines[2]),
        [
            ("   ".into(), String::new()),
            ("https://e.com/x".into(), "https://e.com/x".into())
        ]
    );
}

/// 截断成 `…` 的网址不是链接；已经是链接的字（回答里的）不动；没有网址的行原样。
#[test]
fn cut_urls_existing_links_and_plain_lines_are_left_alone() {
    let mut lines = vec![
        line(&["  ⎿  open https://example.com/very/lo", "…"]),
        line(&["  ⎿  open https://example.com/very/lo…"]),
        StyledLine::new(vec![Span::with_decor(
            "https://e.com/a",
            Style::DEFAULT,
            Decor::link("https://e.com/other"),
        )]),
        line(&["no links here"]),
    ];
    let before = lines.clone();
    mark(&mut lines);
    assert_eq!(lines, before);
}
