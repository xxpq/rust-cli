//! 行内解析的测试。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/InlineParserSuite.scala（feat/tui a2bf878b34）。
use super::ast::Inline::{self, *};
use super::inline_parser::{parse, plain};

fn t(s: &str) -> Inline {
    Text(s.to_owned())
}

/// 排好的行内公式与它的原文。
fn math(text: &str, source: &str) -> Inline {
    Math {
        text: text.to_owned(),
        source: source.to_owned(),
    }
}

fn link(content: Vec<Inline>, dest: &str, title: Option<&str>) -> Inline {
    Link {
        content,
        dest: dest.to_owned(),
        title: title.map(str::to_owned),
    }
}

fn auto(uri: &str, is_email: bool) -> Inline {
    Autolink {
        uri: uri.to_owned(),
        is_email,
    }
}

#[test]
fn plain_text_is_a_single_text_node() {
    assert_eq!(parse("hello world"), vec![t("hello world")]);
    assert_eq!(parse(""), vec![]);
}

#[test]
fn emphasis_and_strong() {
    assert_eq!(parse("*a*"), vec![Emph(vec![t("a")])]);
    assert_eq!(parse("_a_"), vec![Emph(vec![t("a")])]);
    assert_eq!(parse("**a**"), vec![Strong(vec![t("a")])]);
    assert_eq!(parse("__a__"), vec![Strong(vec![t("a")])]);
}

#[test]
fn triple_emphasis_nests_strong_inside_emph() {
    assert_eq!(parse("***a***"), vec![Emph(vec![Strong(vec![t("a")])])]);
}

#[test]
fn nested_emphasis_inside_emphasis() {
    assert_eq!(
        parse("*a **b** c*"),
        vec![Emph(vec![t("a "), Strong(vec![t("b")]), t(" c")])]
    );
}

#[test]
fn intraword_underscore_does_not_emphasise_asterisk_does() {
    assert_eq!(parse("a_b_c"), vec![t("a_b_c")]);
    assert_eq!(parse("a*b*c"), vec![t("a"), Emph(vec![t("b")]), t("c")]);
}

#[test]
fn unmatched_delimiters_stay_literal() {
    assert_eq!(parse("**bold"), vec![t("**bold")]);
    assert_eq!(parse("a * b"), vec![t("a * b")]);
}

#[test]
fn inline_code_span_verbatim_binds_tighter_than_emphasis() {
    assert_eq!(parse("`a*b*c`"), vec![Code("a*b*c".into())]);
    assert_eq!(
        parse("a `code` b"),
        vec![t("a "), Code("code".into()), t(" b")]
    );
}

#[test]
fn code_span_with_double_backticks_holds_a_single_backtick() {
    assert_eq!(parse("``a`b``"), vec![Code("a`b".into())]);
}

#[test]
fn code_span_strips_one_surrounding_space() {
    assert_eq!(parse("` a `"), vec![Code("a".into())]);
}

#[test]
fn unterminated_code_span_is_literal() {
    assert_eq!(parse("`abc"), vec![t("`abc")]);
}

#[test]
fn strikethrough() {
    assert_eq!(parse("~~gone~~"), vec![Strikethrough(vec![t("gone")])]);
}

#[test]
fn strikethrough_wraps_nested_emphasis() {
    assert_eq!(
        parse("~~a **b**~~"),
        vec![Strikethrough(vec![t("a "), Strong(vec![t("b")])])]
    );
}

#[test]
fn inline_link_with_and_without_title() {
    assert_eq!(
        parse("[x](http://e.com)"),
        vec![link(vec![t("x")], "http://e.com", None)]
    );
    assert_eq!(
        parse("[x](http://e.com \"t\")"),
        vec![link(vec![t("x")], "http://e.com", Some("t"))]
    );
}

#[test]
fn link_text_may_itself_contain_emphasis() {
    assert_eq!(
        parse("[*x*](u)"),
        vec![link(vec![Emph(vec![t("x")])], "u", None)]
    );
}

#[test]
fn image() {
    assert_eq!(
        parse("![alt](img.png)"),
        vec![Image {
            alt: "alt".into(),
            dest: "img.png".into(),
            title: None
        }]
    );
}

#[test]
fn angle_bracket_and_email_autolinks() {
    assert_eq!(parse("<http://e.com>"), vec![auto("http://e.com", false)]);
    assert_eq!(parse("<a@b.com>"), vec![auto("a@b.com", true)]);
}

#[test]
fn a_bare_http_address_is_an_autolink_punctuation_after_it_stays_text() {
    assert_eq!(
        parse("see https://example.com/a."),
        vec![t("see "), auto("https://example.com/a", false), t(".")]
    );
    assert_eq!(
        parse("http://e.com, then"),
        vec![auto("http://e.com", false), t(", then")]
    );
    // 括号成对时留在地址里，多出来的右括号不算。
    assert_eq!(
        parse("(https://e.com/a_(b))"),
        vec![t("("), auto("https://e.com/a_(b)", false), t(")")]
    );
}

#[test]
fn a_bare_address_needs_a_word_start_and_something_after_the_scheme() {
    assert_eq!(parse("xhttps://e.com"), vec![t("xhttps://e.com")]);
    assert_eq!(parse("https:// x"), vec![t("https:// x")]);
    assert_eq!(
        parse("[https://a.com](https://b.com)"),
        vec![link(vec![t("https://a.com")], "https://b.com", None)]
    );
    // 没有闭合的 [ 不是链接文字：后面的网址照样是链接。
    assert_eq!(
        parse("[draft https://e.com"),
        vec![t("[draft "), auto("https://e.com", false)]
    );
}

#[test]
fn unmatched_brackets_are_literal_reference_links_unsupported() {
    assert_eq!(parse("[x]"), vec![t("[x]")]);
    assert_eq!(parse("[x][y]"), vec![t("[x][y]")]);
}

#[test]
fn backslash_escapes_punctuation() {
    assert_eq!(parse("\\*not emph\\*"), vec![t("*not emph*")]);
    assert_eq!(parse("a\\\\b"), vec![t("a\\b")]);
}

#[test]
fn backslash_before_a_non_punctuation_char_stays_literal() {
    assert_eq!(parse("a\\b"), vec![t("a\\b")]);
}

#[test]
fn two_trailing_spaces_make_a_hard_break_a_bare_newline_is_soft() {
    assert_eq!(parse("a  \nb"), vec![t("a"), HardBreak, t("b")]);
    assert_eq!(parse("a\nb"), vec![t("a"), SoftBreak, t("b")]);
}

#[test]
fn backslash_newline_is_a_hard_break() {
    assert_eq!(parse("a\\\nb"), vec![t("a"), HardBreak, t("b")]);
}

/* ---- 移植时加的：链表、字符类与 Scala 的 UTF-16 语义 ---- */

#[test]
fn image_alt_flattens_nested_markup_to_plain_text() {
    assert_eq!(
        parse("![a *b* `c`](i.png)"),
        vec![Image {
            alt: "a *b* c".into(),
            dest: "i.png".into(),
            title: None
        }]
    );
    assert_eq!(plain(&parse("a **b** [c](u)  \nd")), "a b c d");
}

#[test]
fn no_links_inside_links() {
    assert_eq!(
        parse("[a [b](u) c](v)"),
        vec![t("[a "), link(vec![t("b")], "u", None), t(" c](v)")]
    );
}

#[test]
fn emphasis_inside_link_text_resolves_before_the_link_closes() {
    assert_eq!(
        parse("*a [b*](u)"),
        vec![t("*a "), link(vec![t("b*")], "u", None)]
    );
    assert_eq!(
        parse("**a [*b*](u) c**"),
        vec![Strong(vec![
            t("a "),
            link(vec![Emph(vec![t("b")])], "u", None),
            t(" c")
        ])]
    );
}

#[test]
fn link_destinations_and_titles() {
    assert_eq!(parse("[x](<a b>)"), vec![link(vec![t("x")], "a b", None)]);
    assert_eq!(
        parse("[x](a(b)c 'q\\'d')"),
        vec![link(vec![t("x")], "a(b)c", Some("q'd"))]
    );
    assert_eq!(
        parse("[x](a\\)b (t))"),
        vec![link(vec![t("x")], "a)b", Some("t"))]
    );
    assert_eq!(parse("[x](a b)"), vec![t("[x](a b)")]);
}

#[test]
fn rule_of_three_keeps_mixed_runs_apart() {
    assert_eq!(parse("*a**"), vec![Emph(vec![t("a")]), t("*")]);
    assert_eq!(parse("**a*"), vec![t("*"), Emph(vec![t("a")])]);
    assert_eq!(parse("*a**b*"), vec![Emph(vec![t("a**b")])]);
}

#[test]
fn continuation_lines_lose_their_leading_spaces() {
    assert_eq!(parse("a\n   b"), vec![t("a"), SoftBreak, t("b")]);
}

#[test]
fn a_character_outside_the_bmp_is_punctuation_like_a_surrogate() {
    // Scala 按 UTF-16 单元看，emoji 的代理项不是字母也不是空白，算标点：
    // "😀_a_" 的 _ 前面是标点，于是能开强调（若按字母算则不能）。
    assert_eq!(parse("😀_a_"), vec![t("😀"), Emph(vec![t("a")])]);
    assert_eq!(parse("中_a_"), vec![t("中_a_")]);
}

#[test]
fn circled_digits_are_punctuation_for_flanking() {
    // ① 是 No：Java 的 isLetterOrDigit 为假（Rust 的 is_alphanumeric 为真）。
    assert_eq!(parse("①_a_"), vec![t("①"), Emph(vec![t("a")])]);
}

#[test]
fn autolink_scheme_needs_a_letter_first() {
    assert_eq!(parse("<1a:b>"), vec![t("<1a:b>")]);
    assert_eq!(parse("<mailto:a@b.c>"), vec![auto("mailto:a@b.c", false)]);
    assert_eq!(parse("<a@b>"), vec![t("<a@b>")]);
}

/* ---- 行内公式（tui-kit.md §3.5；用例取自上游（`vendor/texmath` 的来源）的行内公式测试） ---- */

#[test]
fn dollar_and_paren_delimited_math_is_typeset_into_one_node() {
    assert_eq!(parse("$\\alpha^2$"), vec![math("α²", "$\\alpha^2$")]);
    assert_eq!(
        parse("\\(\\alpha^2\\)"),
        vec![math("α²", "\\(\\alpha^2\\)")]
    );
    assert_eq!(parse("a $x$ b"), vec![t("a "), math("x", "$x$"), t(" b")]);
    assert_eq!(
        parse("$0$"),
        vec![math("0", "$0$")],
        "zero is a formula, not money"
    );
}

#[test]
fn math_keeps_markdown_and_shell_contexts() {
    for (src, want) in [
        (
            "**$\\alpha_1$** and \\$5.00 and $\\beta^2$",
            "α₁ and $5.00 and β²",
        ),
        (
            "`$\\alpha$` $HOME ${HOME} $(echo x) $5 and $10",
            "$\\alpha$ $HOME ${HOME} $(echo x) $5 and $10",
        ),
        ("[x](https://example.com/$HOME) and $\\alpha$", "x and α"),
        ("$\\left. x \\right|$ $\\text{x_y^z}$", " x | x_y^z"),
        ("Value: <$x$> and <\\(\\alpha\\)>.", "Value: <x> and <α>."),
        ("$USD$+$\\alpha$", "$USD$+α"),
        ("Costs $5; $\\alpha$ and $x^2$.", "Costs $5; α and x²."),
        ("$HOME then $\\alpha$", "$HOME then α"),
        (
            "Run echo $$ to print PID. Then $\\alpha$",
            "Run echo $$ to print PID. Then α",
        ),
        ("a $$ b $x$ c $$ d", "a $$ b $x$ c $$ d"),
        ("$x$y and $x $", "$x$y and $x $"),
        ("$a\nb$", "$a b$"),
    ] {
        assert_eq!(plain(&parse(src)), want, "{src}");
    }
}

#[test]
fn unsupported_or_oversized_tex_stays_verbatim() {
    for src in [
        "\\unknown{x}",
        "\\frac{a}",
        "{x",
        "x}",
        "^2",
        "x^2^3",
        "{a+b}^2",
        "\\frac{a}{b}^2",
        "\\sqrt[3]{x}",
        "\\left x",
        "\\text{\\alpha}",
        "\\begin{matrix}a&b\\end{matrix}",
    ] {
        let wrapped = format!("\\({src}\\)");
        assert_eq!(plain(&parse(&wrapped)), wrapped, "{src}");
    }
    let long = format!("${}$", "x".repeat(5000));
    assert_eq!(parse(&long), vec![t(&long)]);
}

#[test]
fn a_formula_the_playhead_is_still_in_is_not_typeset() {
    use crate::markdown::render::HIDDEN;
    let src = format!("$x{HIDDEN}^2$");
    assert_eq!(plain(&parse(&src)), src);
}

/// 普通文字里的网址（docs/specs/rust-cli/tui-select.md §10「怎么认」）：同回答的裸网址，另认引号、`=` 之后的，`"`、
/// `` ` ``、`>` 是结尾，末尾不成对的 `]` 去掉；字母或数字后面的不认；协议后要有字母或数字。
#[test]
fn plain_text_urls_follow_the_bare_url_rule_with_word_starts() {
    use super::inline_parser::plain_urls;
    let found = |text: &str| -> Vec<String> {
        plain_urls(text)
            .into_iter()
            .map(|range| text[range].to_owned())
            .collect()
    };
    assert_eq!(
        found("see https://example.com/a."),
        ["https://example.com/a"]
    );
    assert_eq!(
        found(r#"{"url": "https://example.com/x?q=1","b":"http://e.com"}"#),
        ["https://example.com/x?q=1", "http://e.com"]
    );
    assert_eq!(found("url=https://e.com/a&b=1"), ["https://e.com/a&b=1"]);
    assert_eq!(
        found("`https://e.com/c` and <https://e.com/d>"),
        ["https://e.com/c", "https://e.com/d"]
    );
    assert_eq!(
        found("['https://e.com/1', 'https://e.com/2']"),
        ["https://e.com/1", "https://e.com/2"]
    );
    assert_eq!(found("(https://e.com/a_(b))"), ["https://e.com/a_(b)"]);
    // 全角标点是结尾（回答里同样，见下一个测试）。
    assert_eq!(
        found("打开 https://例子.com/路径，然后。"),
        ["https://例子.com/路径"]
    );
    assert_eq!(found("（见https://e.com/a）"), ["https://e.com/a"]);
    assert!(found("xhttps://e.com https:// http://-x ftp://e.com").is_empty());
}

/// 回答里的裸网址同样到全角标点为止（修改原因 2026-11-12：以前 `，然后` 也算进网址）。
#[test]
fn bare_urls_end_at_full_width_punctuation() {
    assert_eq!(
        parse("见 https://e.com/a，然后"),
        vec![t("见 "), auto("https://e.com/a", false), t("，然后")]
    );
}
