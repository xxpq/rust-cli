//! 公式与 mermaid 图（docs/specs/rust-cli/tui-kit.md §3.5「数学公式与 Mermaid 图」）：行内与独立的公式、图、退回原文，
//! 以及流式时等它闭合。用例取自上游（`vendor/texmath`、`vendor/mermaid` 的来源）的 Markdown 集成测试，改成本工程的排版。
use crate::markdown::render::{
    self, AnswerRenderCache, HIDDEN, MARKDOWN_WORDS, MarkdownWords, Tail, render, rides_tail, waits,
};
use crate::render::{Color, Span, Style, StyledLine, attr, width};
use crate::theme::theme::Theme;

use super::{answer, doc, laid, lines, lines_at, md, rich_theme, v};

const QUADRATIC: &str = "$$\nx=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a}\n$$";

const FLOWCHART: &str = "```mermaid\nflowchart LR\n    A[Request] --> B[Reply]\n```";

/// FLOWCHART 画出的图（上游的画法：连线从节点下边绕过去）。
const FLOWCHART_ROWS: [&str; 8] = [
    "┌─────────┐  ┌───────┐",
    "│ Request │  │ Reply │",
    "└┬────────┘  └┬──────┘",
    " │            ▲",
    " │            │",
    " │            │",
    " │            │",
    " └────────────┘",
];

fn streaming() -> Tail {
    Tail::new(Some(Span::new("█", Style::fg(rich_theme().brand))))
}

fn ended() -> Tail {
    Tail::default().with_ended(true)
}

fn plain(ls: &[StyledLine]) -> Vec<String> {
    ls.iter().map(StyledLine::plain).collect()
}

/* ---- 行内公式 ---- */

#[test]
fn inline_math_is_typeset_in_the_line_and_unsupported_tex_stays_verbatim() {
    let src = "Inline $\\alpha^2 + \\beta_{10}$ on $\\mathbb{R}^n$.\n\
               Root $\\sqrt{x^2+y^2}$ and fraction $a/\\frac{b}{c}$.\n\
               Unsupported: $\\unknown{x_y}$ and $\\frac{a}{b}^2$.\n\
               Angles: <$x$>, <\\(\\alpha\\)>, and <$\\unknown{x}$>.\n\
               Code: `$\\alpha$`; money: $5 and $10; shell: $HOME.";
    assert_eq!(
        lines_at(&md(src), 80),
        v(&[
            "   Inline α² + β₁₀ on ℝⁿ. Root √(x²+y²) and fraction a/((b)/(c)). Unsupported:",
            "   $\\unknown{x_y}$ and $\\frac{a}{b}^2$. Angles: <x>, <α>, and <$\\unknown{x}$>.",
            "   Code: $\\alpha$; money: $5 and $10; shell: $HOME.",
        ])
    );
}

#[test]
fn an_inline_formula_does_not_break_in_the_middle_when_it_fits_a_line() {
    // 上游 unicode_math_inline_narrow_snapshot：同上游的折法（多了正文的三列缩进）。
    assert_eq!(
        lines_at(
            &md("Words $\\alpha^2 + \\beta_1$ more words and $\\sqrt{x}$."),
            16
        ),
        v(&["   Words α² + β₁", "   more words", "   and √(x)."])
    );
    // 放不下一行剩下的地方时公式整段移到下一行，不在中间的空格处折开（同行内代码）。
    assert_eq!(
        lines_at(&md("Words go $\\alpha^2 + \\beta_1$ here"), 16),
        v(&["   Words go", "   α² + β₁ here"])
    );
}

#[test]
fn an_inline_formula_is_in_the_body_ink() {
    let rich = rich_theme();
    let ls = laid(&render(&doc("a $\\alpha$ b"), &rich), 60);
    let alpha = ls[0].spans.iter().find(|sp| sp.text == "α").unwrap();
    assert_eq!(alpha.style.fg_color(), Color::Default, "{alpha:?}");
    assert!(alpha.style.has_attr(attr::WHOLE));
}

/* ---- 独立的公式 ---- */

#[test]
fn a_display_formula_is_its_own_block_and_fractions_stack() {
    assert_eq!(
        lines(&md(&format!("intro\n\n{QUADRATIC}\n\nafter"))),
        v(&[
            "   intro",
            "",
            "     -b±√(b²-4ac)",
            "   x=────────────",
            "          2a",
            "",
            "   after",
        ])
    );
    assert_eq!(
        lines(&md("$$\\frac{1}{2}\\frac{3}{4}$$")),
        v(&["   1 3", "   ─ ─", "   2 4"])
    );
    assert_eq!(
        lines(&md("\\[\n\\int_0^1 x^2\\,dx=\\frac{1}{3}\n\\]")),
        v(&["             1", "   ∫₀¹ x² dx=─", "             3"])
    );
}

#[test]
fn a_display_formula_too_wide_for_the_width_is_its_source_wrapped_like_prose() {
    // 上游：放不下的分数不拆开（会误读），显示原文。
    let ls = lines_at(&md("$$\\frac{abcdefghij}{k}$$"), 12);
    assert_eq!(ls, v(&["   $$\\frac{a", "   bcdefghij", "   }{k}$$"]));
    assert_eq!(
        lines_at(&md("$$\\frac{abcdefghij}{k}$$"), 40),
        v(&["   abcdefghij", "   ──────────", "       k"])
    );
}

#[test]
fn a_display_formula_in_a_nested_list_is_drawn_after_the_lists_indent() {
    assert_eq!(
        lines(&md("- a\n  - b\n\n    $$\\frac{1234567890}{x}$$")),
        v(&[
            "   • a",
            "",
            "     ◦ b",
            "",
            "       1234567890",
            "       ──────────",
            "           x",
        ])
    );
}

#[test]
fn unsupported_or_unclosed_display_math_and_shell_dollars_stay_verbatim() {
    // 上游 unicode_math_pending_source_and_pid_boundaries。
    assert_eq!(
        lines(&md("$$\nx\n$$ trailing\n$$\n\nAfter $\\alpha$.")),
        v(&[
            "   $$",
            "   x",
            "   $$ trailing",
            "   $$",
            "",
            "   After α."
        ])
    );
    assert_eq!(
        lines(&md("echo $$\n\n$$\nx^2\n$$\n\nAfter $\\alpha$.")),
        v(&["   echo $$", "", "   x²", "", "   After α."])
    );
    assert_eq!(
        lines(&md("$$\nprice=\\$$$\n\nAfter $\\alpha$.\n\n$$\\beta$$")),
        v(&["   $$", "   price=\\$$$", "", "   After α.", "", "   β"])
    );
    assert_eq!(lines(&md("$$\nx^2\n\n+y^2\n$$")), v(&["   x² +y²"]));
    for src in ["$$\n# x\n- y", "\\[\n# x\n- y"] {
        assert_eq!(
            lines(&md(src)),
            src.lines().map(|l| format!("   {l}")).collect::<Vec<_>>(),
            "{src}: an unclosed formula runs to the end as its source"
        );
    }
    assert_eq!(
        lines(&md("$$x$$ trailing")),
        v(&["   $$x$$ trailing"]),
        "text after the closer: a paragraph"
    );
}

#[test]
fn an_oversized_display_does_not_hide_the_inline_math_after_it() {
    for (open, close) in [("$$", "$$"), ("\\[", "\\]")] {
        let src = format!(
            "{open}\n{}\n{close}\n\nAfter $\\alpha$.\n",
            "x".repeat(5000)
        );
        let ls = lines(&md(&src));
        assert_eq!(ls.last().unwrap(), "   After α.", "{open}");
        assert!(
            ls[0].starts_with(&format!("   {open}")),
            "{open}: {}",
            ls[0]
        );
    }
}

/* ---- mermaid ---- */

#[test]
fn a_mermaid_block_is_drawn_as_a_diagram_on_the_code_panel() {
    let rich = rich_theme();
    let ls = plain(&laid(&render(&doc(FLOWCHART), &rich), 60));
    assert!(ls[0].trim().chars().all(|c| c == '▄'), "{ls:?}");
    assert!(
        ls.last().unwrap().trim().chars().all(|c| c == '▀'),
        "{ls:?}"
    );
    assert!(ls[1].trim_end().ends_with("mermaid"), "{:?}", ls[1]);
    for (row, want) in ls[1..=FLOWCHART_ROWS.len()].iter().zip(FLOWCHART_ROWS) {
        assert!(
            row.starts_with(&format!("    {want}")),
            "{row:?} vs {want:?}"
        );
    }
}

#[test]
fn plain_a_mermaid_diagram_is_the_label_line_then_the_rail() {
    let mut want = vec!["   mermaid".to_owned()];
    want.extend(FLOWCHART_ROWS.iter().map(|r| format!("   ▏ {r}")));
    assert_eq!(lines(&md(FLOWCHART)), want);
    assert_eq!(
        lines(&md(
            "- Diagram:\n\n  ```mermaid\n  flowchart TD\n      A[请求] --> B[Réponse]\n  ```"
        ))[2..5],
        v(&["     mermaid", "     ▏ ┌─────────┐", "     ▏ │ 请求    │"]),
        "inside a list item, and CJK labels keep the box aligned"
    );
}

#[test]
fn rich_nodes_are_in_the_type_ink_edges_in_fg3_labels_in_the_body_ink() {
    let rich = rich_theme();
    let ls = laid(&render(&doc(FLOWCHART), &rich), 60);
    let span = |text: &str| {
        ls.iter()
            .flat_map(|l| l.spans.iter())
            .find(|sp| sp.text.contains(text))
            .unwrap_or_else(|| panic!("{text}"))
            .clone()
    };
    assert_eq!(span("┌───").style.fg_color(), rich.syntax.ty);
    assert_eq!(span("▲").style.fg_color(), rich.fg3.fg_color());
    assert_eq!(span("Request").style.fg_color(), Color::Default);
    let plain_ls = laid(&render(&doc(FLOWCHART), &Theme::PLAIN), 60);
    assert!(
        plain_ls
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|sp| sp.text.contains('┌'))
            .all(|sp| sp.style.fg_color() == Color::Default),
        "plain: nodes have no color"
    );
}

/// 画不出的图：原因一句（按词折行）在上面，下面是代码块。给出（原因连成的一句，原因之后的行）。
fn said_and_below(ls: &[String]) -> (String, Vec<String>) {
    let k = ls
        .iter()
        .position(|l| l.trim() == "mermaid")
        .unwrap_or_else(|| panic!("no code block: {ls:?}"));
    let said = ls[..k]
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ");
    (said, ls[k..].to_vec())
}

fn source_block(code: &str) -> Vec<String> {
    let mut want = vec!["   mermaid".to_owned()];
    want.extend(code.lines().map(|l| format!("   ▏ {l}")));
    want
}

#[test]
fn a_mermaid_block_that_cannot_be_drawn_says_why_above_its_source() {
    // 用户决定 2026-11-12（同上游 mermaid_fallback_notices_preserve_source）：修改前只有代码块，不写原因。
    for (code, why) in [
        ("pie\n\"Cats\": 2", MARKDOWN_WORDS.mermaid_unsupported),
        (
            "flowchart TD\nP --> Q\nA[(Database)]",
            MARKDOWN_WORDS.mermaid_unsupported,
        ),
        (
            "flowchart TD\nA[This label exceeds the forty column limit]",
            MARKDOWN_WORDS.mermaid_limit,
        ),
    ] {
        let ls = lines(&md(&format!("```mermaid\n{code}\n```")));
        assert_eq!(
            said_and_below(&ls),
            (why.to_owned(), source_block(code)),
            "{code}"
        );
    }
    let narrow = lines_at(&md(FLOWCHART), 16);
    let (said, below) = said_and_below(&narrow);
    assert_eq!(said, MARKDOWN_WORDS.mermaid_too_wide, "{narrow:?}");
    assert!(
        !below.iter().any(|l| l.contains('┌')) && below[1].contains("flowcha"),
        "{narrow:?}"
    );
    assert!(
        narrow.iter().all(|l| width::string_width(l) <= 16),
        "the reason wraps to the width: {narrow:?}"
    );
}

#[test]
fn an_unclosed_mermaid_fence_is_its_source_without_a_reason() {
    // 上游 mermaid_unclosed_blocks_keep_source_without_notice。
    assert_eq!(
        lines(&md("```mermaid\nflowchart LR\nA --> B")),
        source_block("flowchart LR\nA --> B")
    );
}

#[test]
fn rich_the_reason_is_in_fg3_above_the_panel() {
    let rich = rich_theme();
    let ls = laid(
        &render(&doc("```mermaid\npie\n\"Cats\": 2\n```"), &rich),
        80,
    );
    let reason = ls[0]
        .spans
        .iter()
        .find(|sp| sp.text.contains("Mermaid"))
        .unwrap_or_else(|| panic!("{:?}", plain(&ls)));
    assert_eq!(reason.style.fg_color(), rich.fg3.fg_color());
    assert!(
        ls[1].plain().trim().chars().all(|c| c == '▄'),
        "{:?}",
        plain(&ls)
    );
}

#[test]
fn the_reason_is_in_the_words_the_caller_gives() {
    const WORDS: MarkdownWords = MarkdownWords {
        mermaid_unsupported: "不支持",
        mermaid_too_wide: "放不下",
        mermaid_limit: "超限",
    };
    let d = doc("```mermaid\npie\n\"Cats\": 2\n```\n\nafter");
    let said = |words: &MarkdownWords, cache: &mut AnswerRenderCache| {
        let e = render::answer_cached(&d, None, cache, &Theme::PLAIN, words);
        lines_at(&e, 100)[0].clone()
    };
    let mut cache = AnswerRenderCache::new();
    assert_eq!(said(&WORDS, &mut cache), "   不支持");
    assert_eq!(
        said(&MARKDOWN_WORDS, &mut cache),
        format!("   {}", MARKDOWN_WORDS.mermaid_unsupported),
        "another language does not reuse the cached block"
    );
    let plain_answer = render::answer(&d, None, &Theme::PLAIN, &WORDS);
    assert_eq!(lines(&plain_answer)[0], "   不支持");
}

/* ---- 流式（用户决定 2026-11-12：先流式显示源码，闭合后换成图） ---- */

/// `ls` 里有一段标成代理写的字、含 `text` 的段。
fn written_text(ls: &[StyledLine], text: &str) -> bool {
    ls.iter()
        .flat_map(|l| l.spans.iter())
        .any(|sp| sp.text.contains(text) && sp.style.has_attr(attr::WRITTEN))
}

#[test]
fn an_arriving_display_formula_streams_its_source_once_closed_it_becomes_the_formula() {
    // 修改前：没闭合时只画笔尖（`waits` 为真），原文不显示。
    for (open, want) in [
        (
            "intro\n\n$$\nx=\\frac{1}{2",
            v(&["   intro", "", "   $$", "   x=\\frac{1}{2█"]),
        ),
        (
            "intro\n\n$$x = \\frac{1}{2",
            v(&["   intro", "", "   $$x = \\frac{1}{2█"]),
        ),
        ("intro\n\n\\[\nx", v(&["   intro", "", "   \\[", "   x█"])),
    ] {
        let d = doc(open);
        assert!(!waits(&d), "{open}");
        assert!(rides_tail(&d), "{open}");
        assert_eq!(
            lines(&answer(&d, Some(&streaming()), &Theme::PLAIN)),
            want,
            "{open}"
        );
        let mut at_end = want.clone();
        let last = at_end.pop().unwrap();
        at_end.push(last.trim_end_matches('█').to_owned());
        assert_eq!(
            lines(&answer(&d, Some(&ended()), &Theme::PLAIN)),
            at_end,
            "{open}: at the end the source stays"
        );
    }
    // 原文是代理写的字：逐字落定，笔骑在末尾。
    let rich = rich_theme();
    let ls = laid(
        &answer(&doc("$$\nx=\\frac{1}{2"), Some(&streaming()), &rich),
        60,
    );
    assert!(written_text(&ls, "frac"), "{:?}", plain(&ls));
    let closed = doc("intro\n\n$$\nx=\\frac{1}{2}\n$$");
    assert!(!waits(&closed));
    assert!(!rides_tail(&closed), "the formula draws no nib of its own");
    let ls = laid(&answer(&closed, Some(&streaming()), &rich), 60);
    assert_eq!(
        plain(&ls),
        v(&["   intro", "", "     1", "   x=─", "     2"])
    );
    assert!(
        !ls[2..]
            .iter()
            .any(|l| l.spans.iter().any(|sp| sp.style.has_attr(attr::WRITTEN))),
        "a formula lands whole, not letter by letter"
    );
}

#[test]
fn a_formula_the_playhead_is_still_in_shows_its_source_up_to_the_playhead() {
    // 闭合的定界符已在排版的文字里，但播放头还在最后一个词里：不排一个缺字的公式。
    let d = doc(&format!("$$x +\ny{HIDDEN}$$"));
    assert!(rides_tail(&d));
    let ls = lines(&answer(&d, Some(&streaming()), &Theme::PLAIN));
    let ls: Vec<String> = ls
        .iter()
        .map(|l| l.replace('\u{a0}', " ").trim_end().to_owned())
        .collect();
    assert_eq!(ls, v(&["   $$x +", "   y█"]));
}

#[test]
fn an_unsupported_formula_that_closes_keeps_its_source_and_only_the_nib_leaves() {
    let open = lines(&answer(
        &doc("$$\n\\unknown{x}"),
        Some(&streaming()),
        &Theme::PLAIN,
    ));
    assert_eq!(open, v(&["   $$", "   \\unknown{x}█"]));
    let closed = doc("$$\n\\unknown{x}\n$$");
    assert!(!rides_tail(&closed));
    assert_eq!(
        lines(&answer(&closed, Some(&streaming()), &Theme::PLAIN)),
        v(&["   $$", "   \\unknown{x}", "   $$"])
    );
}

#[test]
fn an_arriving_mermaid_fence_streams_as_a_code_block_once_closed_it_becomes_the_diagram() {
    // 修改前：没闭合时只画笔尖。现在同别的代码块：写到播放头为止，笔尖骑在代码末尾。
    let open = doc("intro\n\n```mermaid\nflowchart LR\n    A[Request] --> B[Re");
    assert!(!waits(&open));
    assert!(rides_tail(&open));
    assert_eq!(
        lines(&answer(&open, Some(&streaming()), &Theme::PLAIN)),
        v(&[
            "   intro",
            "",
            "   mermaid",
            "   ▏ flowchart LR",
            "   ▏     A[Request] --> B[Re█",
        ])
    );
    let rich = rich_theme();
    let ls = laid(&answer(&open, Some(&streaming()), &rich), 60);
    assert!(written_text(&ls, "flowchart"), "{:?}", plain(&ls));
    let at_end = lines(&answer(&open, Some(&ended()), &Theme::PLAIN));
    assert_eq!(
        at_end.last().unwrap(),
        "   ▏     A[Request] --> B[Re",
        "at the end the source stays: {at_end:?}"
    );
    let closed = doc(&format!("intro\n\n{FLOWCHART}"));
    assert!(!waits(&closed) && !rides_tail(&closed));
    let ls = laid(&answer(&closed, Some(&streaming()), &rich), 60);
    assert!(
        ls.iter().any(|l| l.plain().contains("│ Request │")),
        "{:?}",
        plain(&ls)
    );
    assert!(
        !ls.iter().any(|l| l.plain().contains('█')),
        "{:?}",
        plain(&ls)
    );
    assert!(
        !ls[2..]
            .iter()
            .any(|l| l.spans.iter().any(|sp| sp.style.has_attr(attr::WRITTEN))),
        "the diagram lands whole"
    );
}
