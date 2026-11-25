//! 右边一组键按对拆开（[`super::key_parts`]，docs/specs/rust-cli/tui-kit.md §4.0「页的键」）。
use crate::render::{Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::super::key_hints;
use super::{Part, key_parts, line};

fn theme() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn keys(theme: &Theme) -> Vec<Span> {
    key_hints::spans(
        theme,
        &[
            ("⇥".to_owned(), "plan".to_owned()),
            ("←→".to_owned(), "range".to_owned()),
            ("esc".to_owned(), "back".to_owned()),
        ],
    )
}

fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_ref()).collect::<String>()
}

#[test]
fn each_pair_is_a_part_and_the_last_ranks_highest() {
    let theme = theme();
    let parts = key_parts("note", keys(&theme), 95);
    let ranks: Vec<i32> = parts.iter().map(|p| p.rank).collect();
    assert_eq!(ranks, [93, 94, 95]);
    assert!(parts.iter().all(|p| p.right));
    assert_eq!(parts[0].glue, None);
    assert_eq!(
        plain(parts[1].glue.as_deref().unwrap()),
        key_hints::PAIR_GAP
    );
    assert_eq!(plain(&parts[2].spans), "esc back");
    // 一句说明不拆。
    let said = key_parts("note", vec![Span::new("Copied the page", theme.fg2)], 95);
    assert_eq!(said.len(), 1);
    assert_eq!(said[0].rank, 95);
}

/// 窄时从前面一对一对地走，esc 那一对最后走；左边名次更高的模式留到最后。
#[test]
fn narrow_lines_keep_the_last_pairs() {
    let theme = theme();
    let left = vec![Part::new(
        "mode",
        vec![Span::new("Default", theme.fg3)],
        100,
    )];
    let note = key_parts("note", keys(&theme), 95);
    let at = |width: usize| {
        plain(&line(&left, &[], &note, width, Style::DEFAULT))
            .trim_end()
            .to_owned()
    };
    assert!(
        at(60).ends_with("⇥ plan  ←→ range  esc back"),
        "{:?}",
        at(60)
    );
    let mut tails = Vec::new();
    for width in (8..=60).rev() {
        let text = at(width);
        let tail = [
            "⇥ plan  ←→ range  esc back",
            "←→ range  esc back",
            "esc back",
        ]
        .iter()
        .position(|t| text.ends_with(t))
        .unwrap_or(3);
        tails.push(tail);
    }
    assert!(tails.windows(2).all(|w| w[0] <= w[1]), "{tails:?}");
    for kept in 0..=3 {
        assert!(tails.contains(&kept), "{kept} never shown: {tails:?}");
    }
    assert!(
        at(8).starts_with("   Default") || at(8).is_empty(),
        "{:?}",
        at(8)
    );
}
