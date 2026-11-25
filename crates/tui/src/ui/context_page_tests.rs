//! 设置的 Context 与 Details（tui-panels.md §4.2、§4.3；设计稿 D「方格图」）：Context 是方格图与右边的图例、一句话；Details
//! 是大纲的列、余量、建议；窄时图例放到下面、一句话变短、去掉 note 与指向；没有窗口与超限。行照以前的页从第 3 列起（设置的
//! 窗格排好之后去掉这 3 列，`settings/live.rs`）。样本见 `context_tree_tests.rs` 的 [`sample`]。
use std::collections::BTreeSet;

use rust_cli_kernel_api::context_usage::ContextUsage;
use rust_cli_tui_kit::markdown::render::SETTLED;
use rust_cli_tui_kit::render::StyledLine;

use super::*;
use crate::ui::context_tree::tests::sample;
use crate::ui::ink::Ink;
use crate::ui::probe::ColorLevel;

fn ink() -> Ink {
    Ink::new(
        ColorLevel::TrueColor,
        Some((255, 255, 255)),
        Some((0, 0, 0)),
    )
}

fn path(parts: &[&str]) -> Path {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn text(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

/// Context 的行，与 Details 的行和选中的行。
fn drawn(
    usage: Option<&ContextUsage>,
    width: usize,
    open: &[&[&str]],
) -> (Vec<String>, Vec<String>, Option<usize>) {
    let ink = ink();
    let open: BTreeSet<Path> = open.iter().map(|p| path(p)).collect();
    let inputs = Inputs {
        theme: &ink.theme,
        lang: crate::ui::i18n::Lang::En,
        width,
        usage,
        failed: None,
        open: &open,
        chosen: &[],
        age: &|_| SETTLED,
        places: Places::default(),
    };
    let details = details(&inputs);
    (
        text(&overview(&inputs)),
        text(&details.lines),
        details.chosen,
    )
}

/// Details 的大纲或余量里名字是 `name` 的行。
fn row<'a>(lines: &'a [String], name: &str) -> &'a str {
    lines
        .iter()
        .find(|l| {
            l.trim_start()
                .trim_start_matches(['▸', '▾', '■', '╌', '─', ' '])
                .starts_with(name)
        })
        .unwrap_or_else(|| panic!("no row {name}: {lines:#?}"))
}

fn col(line: &str, text: &str) -> usize {
    let at = line
        .find(text)
        .unwrap_or_else(|| panic!("{text} in {line}"));
    rust_cli_tui_kit::render::width::string_width(&line[..at])
}

/// 设计稿 D 的一帧（100 列）：Context 是十行方格（每格 `■ `，从第 3 列起），图例在第 28 列，一句话也从第 3 列起；Details 是
/// 大纲的五个组（选中第一行）、余量两行。
#[test]
fn context_is_the_grid_and_the_sentence_and_details_the_outline() {
    let usage = sample();
    let (lines, details, chosen) = drawn(Some(&usage), 100, &[]);
    // 没有面包屑：设置页有自己的（以前是 `Stats  ›  Context` 与两个标签）。
    assert_eq!(lines.len(), 12, "{lines:#?}");
    let grid: Vec<&String> = lines[0..10].iter().collect();
    for line in &grid {
        assert_eq!(col(line, " "), 0);
        assert!(
            line[3..].starts_with('■') || line[3..].starts_with('□'),
            "{line}"
        );
    }
    // 45 + 7 + 5 + 5 + 1 格是五个组，17 格空闲，20 格缓冲。
    let squares: String = grid
        .iter()
        .map(|l| l.chars().skip(3).take(20).collect::<String>())
        .collect();
    assert_eq!(squares.matches('■').count(), 63, "{squares}");
    assert_eq!(squares.matches('□').count(), 37, "{squares}");
    assert_eq!(col(grid[0], "126k of 200k   62%"), 28);
    assert_eq!(
        grid[1].trim_start().split("  ").last(),
        Some("compacts at 160k")
    );
    // 名字补到 10 列，数与占比各右对齐 5 列。
    assert!(grid[3].ends_with("■ Messages    89k  45%"), "{}", grid[3]);
    assert!(grid[7].ends_with("■ Skills     1.2k   1%"), "{}", grid[7]);
    assert!(grid[8].ends_with("□ Free        34k  17%"), "{}", grid[8]);
    assert!(grid[9].ends_with("□ Buffer      40k  20%"), "{}", grid[9]);
    let sentence = &lines[11];
    assert_eq!(
        sentence.trim_start(),
        "37k comes with every request  ·  /compact can free up to 89k"
    );
    assert_eq!(col(sentence, "37k"), 3);
    // 大纲：开合的记号在第 3 列、组的 `■` 在第 5 列、名字第 7 列，note 第 34 列，数右对齐到 60，占比到 66，指向第 69 列。
    let messages = row(&details, "Messages ");
    assert_eq!(chosen, Some(0));
    assert_eq!(details[0], messages);
    assert_eq!(col(messages, "▸"), 3);
    assert_eq!(col(messages, "Messages"), 7);
    assert_eq!(col(messages, "4 kinds"), 34);
    assert_eq!(col(messages, "89k") + 3, 60);
    assert_eq!(col(messages, "45%") + 3, 66);
    let mcp = row(&details, "MCP ");
    assert_eq!(col(mcp, "/mcp"), 69);
    let names: Vec<String> = details[0..5]
        .iter()
        .map(|l| {
            l.chars()
                .skip(7)
                .collect::<String>()
                .split("  ")
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(names, ["Messages", "Tools", "MCP", "System", "Skills"]);
    assert!(row(&details, "Autocompact buffer").contains("for compacting"));
    assert!(row(&details, "Free ").ends_with("34k   17%"));
}

/// 打开的行：第二层名字第 9 列（能开的 `▸` 在第 7 列），第三层第 11 列，第三层不写占比；按需加载的行在本层最后。
#[test]
fn open_rows_indent_and_the_third_level_has_no_share() {
    let usage = sample();
    let (_, lines, _) = drawn(
        Some(&usage),
        100,
        &[
            &["messages"],
            &["messages", "Tool results"],
            &["tools"],
            &["tools", "System tools"],
        ],
    );
    let results = row(&lines, "Tool results");
    assert_eq!((col(results, "▾"), col(results, "Tool results")), (7, 9));
    assert!(results.contains("68 calls"), "{results}");
    let read = row(&lines, "Read ");
    assert_eq!(col(read, "Read"), 11);
    assert!(read.contains("23 calls") && read.ends_with("41k"), "{read}");
    let replies = row(&lines, "Replies");
    assert_eq!(col(replies, "Replies"), 9);
    assert!(replies.ends_with('%'), "{replies}");
    let more = row(&lines, "2 more on demand");
    assert!(
        more.contains("not loaded") && more.ends_with("5.3k"),
        "{more}"
    );
    let after = lines.iter().position(|l| l == more).expect("the row") + 1;
    assert!(lines[after].contains("Custom agents"), "{lines:#?}");
}

/// 窄的终端：图例在方格图下面（空一行），一句话的短写法，没有 note 与指向。
#[test]
fn narrow_terminals_move_the_legend_and_drop_columns() {
    let usage = sample();
    let (lines, details, _) = drawn(Some(&usage), 50, &[]);
    assert_eq!(lines[10], "");
    assert_eq!(lines[11].trim_start(), "126k of 200k   62%");
    assert!(
        lines
            .iter()
            .any(|l| l.trim_start() == "37k every request  ·  89k can go"),
        "{lines:#?}"
    );
    let mcp = row(&details, "MCP ");
    assert!(!mcp.contains("/mcp") && !mcp.contains("servers"), "{mcp}");
    assert_eq!(col(mcp, "11k") + 3, 40);
}

/// 窗口 ≥ 1M：每行 20 格，图例在第 48 列。
#[test]
fn a_million_token_window_has_twenty_columns() {
    let mut usage = sample();
    usage.raw_max_tokens = 1_000_000;
    let (lines, _, _) = drawn(Some(&usage), 100, &[]);
    assert_eq!(
        lines[0].chars().filter(|c| matches!(c, '■' | '□')).count(),
        20
    );
    assert_eq!(col(&lines[0], "126k of 1M"), 48);
}

/// 建议：接近满（≥ 80 %）是琥珀色的 `⚠` 与说明；规则文件大时是 `◎` 与能省的量；先警告。
#[test]
fn suggestions_cover_a_full_window_and_large_memory_files() {
    let mut usage = sample();
    usage.percentage = 85;
    usage.memory_files[0].tokens = 12_000;
    let (_, lines, _) = drawn(Some(&usage), 100, &[]);
    let at = lines
        .iter()
        .position(|l| l == "   Suggestions")
        .expect("suggestions");
    assert_eq!(lines[at + 1], "   ⚠ Context is 85% full");
    assert!(lines[at + 2].starts_with("     Autocompact will trigger soon"));
    let memory = lines
        .iter()
        .find(|l| l.contains("◎ Memory files use"))
        .expect("memory");
    assert!(
        memory.contains("13k tokens (6%)  ·  saves ~3.9k"),
        "{memory}"
    );
    let (_, calm, _) = drawn(Some(&sample()), 100, &[]);
    assert!(!calm.iter().any(|l| l.contains("Suggestions")));
}

/// 模型没有窗口：一行用量代替方格图，大纲没有占比、没有余量；超出窗口时 Context 的第一行是失败的一行。
#[test]
fn an_unknown_window_says_so_and_over_the_limit_fails() {
    let mut usage = sample();
    usage.raw_max_tokens = 0;
    let (lines, details, _) = drawn(Some(&usage), 100, &[]);
    assert!(
        lines[0].contains("126k tokens   the model does not say its window"),
        "{lines:#?}"
    );
    assert!(!row(&details, "Messages").contains('%'));
    assert!(!details.iter().any(|l| l.contains("Autocompact buffer")));
    let mut over = sample();
    over.total_tokens = 230_000;
    let (lines, _, _) = drawn(Some(&over), 100, &[]);
    assert!(
        lines[0].contains("✗ Context is 30k tokens past the 200k-token"),
        "{lines:#?}"
    );
    // 用掉的超过窗口：方格都是组的。
    let squares: usize = lines[2..12].iter().map(|l| l.matches('■').count()).sum();
    assert!(squares >= 63, "{lines:#?}");
}

/// 答案到之前两页都是一行 `Counting…`（以前先画卡的 Context 行，卡删了）。
#[test]
fn before_the_answer_both_pages_say_counting() {
    let (lines, details, chosen) = drawn(None, 100, &[]);
    assert_eq!(lines, ["   Counting…"]);
    assert_eq!(details, ["   Counting…"]);
    assert_eq!(chosen, None);
}
