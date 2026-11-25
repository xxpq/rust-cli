//! 行级 diff：期望值都是 jsdiff 9 的输出（`structuredPatch("f", "f", old, new, undefined, undefined, {context})` 的 `hunks`；
//! 编辑距离的上限照 `{maxEditLength: 10000}`）。

use super::{Hunk, NO_NEWLINE_MARKER, common_ends, structured_patch};

fn lines(count: usize, line: impl Fn(usize) -> String) -> String {
    (1..=count).map(|n| line(n) + "\n").collect()
}

fn hunk(old: (usize, usize), new: (usize, usize), lines: &[&str]) -> Hunk {
    Hunk {
        old_start: old.0,
        old_lines: old.1,
        new_start: new.0,
        new_lines: new.1,
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
    }
}

fn patch(old: &str, new: &str, context: usize) -> Vec<Hunk> {
    structured_patch(old, new, context).expect("within the edit limit")
}

/// 新文件与删光：一侧没有行时那一侧的行数是 0、起始行仍是 1（jsdiff 只在拼文字时减一）。
#[test]
fn a_new_file_and_a_removed_file() {
    assert_eq!(
        patch("", "one\ntwo\n", 4),
        [hunk((1, 0), (1, 2), &["+one", "+two"])]
    );
    assert_eq!(
        patch("x\ny\n", "", 4),
        [hunk((1, 2), (1, 0), &["-x", "-y"])]
    );
}

#[test]
fn a_changed_line_comes_with_four_lines_of_context() {
    let old = lines(12, |n| format!("l{n}"));
    let new = old.replacen("l6\n", "L6\n", 1);
    assert_eq!(
        patch(&old, &new, 4),
        [hunk(
            (2, 9),
            (2, 9),
            &[
                " l2", " l3", " l4", " l5", "-l6", "+L6", " l7", " l8", " l9", " l10"
            ]
        )]
    );
    assert_eq!(
        patch(&old, &new, 0),
        [hunk((6, 1), (6, 1), &["-l6", "+L6"])]
    );
}

/// 两处改动之间的相同行不超过两倍上下文时并成一个 hunk，否则是两个。
#[test]
fn changes_merge_when_the_lines_between_fit_the_context() {
    let old = lines(30, |n| format!("k{n}"));
    let far = old
        .replacen("k3\n", "K3\n", 1)
        .replacen("k20\n", "K20\n", 1);
    assert_eq!(
        patch(&old, &far, 4),
        [
            hunk(
                (1, 7),
                (1, 7),
                &[" k1", " k2", "-k3", "+K3", " k4", " k5", " k6", " k7"]
            ),
            hunk(
                (16, 9),
                (16, 9),
                &[
                    " k16", " k17", " k18", " k19", "-k20", "+K20", " k21", " k22", " k23", " k24"
                ]
            ),
        ]
    );
    let eight_between = old
        .replacen("k3\n", "K3\n", 1)
        .replacen("k12\n", "K12\n", 1);
    let merged = patch(&old, &eight_between, 4);
    assert_eq!(merged.len(), 1);
    assert_eq!((merged[0].old_start, merged[0].old_lines), (1, 16));
    assert_eq!(merged[0].lines.len(), 18);
    let wide = patch(&old, &far, 8);
    assert_eq!(wide.len(), 1);
    assert_eq!((wide[0].old_start, wide[0].old_lines), (1, 28));
}

#[test]
fn an_insertion_at_the_start_and_a_removal_at_the_end() {
    let old = lines(12, |n| format!("l{n}"));
    let new = format!("new\n{}", old.replacen("l12\n", "", 1));
    assert_eq!(
        patch(&old, &new, 4),
        [
            hunk((1, 4), (1, 5), &["+new", " l1", " l2", " l3", " l4"]),
            hunk((8, 5), (9, 4), &[" l8", " l9", " l10", " l11", "-l12"]),
        ]
    );
}

/// 最后一行带不带换行是两个不同的行；没有换行的那一行之后是标记。
#[test]
fn a_missing_final_newline_is_marked() {
    assert_eq!(
        patch("a\nb", "a\nc", 4),
        [hunk(
            (1, 2),
            (1, 2),
            &[" a", "-b", NO_NEWLINE_MARKER, "+c", NO_NEWLINE_MARKER]
        )]
    );
    assert_eq!(
        patch("a", "a\n", 4),
        [hunk((1, 1), (1, 1), &["-a", NO_NEWLINE_MARKER, "+a"])]
    );
}

/// `\r\n` 的行只去掉 `\n`；非 ASCII 原样。
#[test]
fn crlf_and_non_ascii_lines() {
    assert_eq!(
        patch("a\r\nb\r\n", "a\r\nc\r\n", 4),
        [hunk((1, 2), (1, 2), &[" a\r", "-b\r", "+c\r"])]
    );
    assert_eq!(
        patch("你好\n😀\n", "你好\n😀!\n", 4),
        [hunk((1, 2), (1, 2), &[" 你好", "-😀", "+😀!"])]
    );
}

#[test]
fn no_change_has_no_hunk() {
    assert_eq!(patch("same\n", "same\n", 4), []);
    assert_eq!(patch("", "", 4), []);
}

/// 编辑距离到上限（10000）还算，超过时不算（jsdiff 的 `maxEditLength` 同样只在超过时放弃）。
#[test]
fn past_the_edit_limit_there_is_no_patch() {
    let at_limit = patch("", &lines(10_000, |n| n.to_string()), 4);
    assert_eq!(at_limit.len(), 1);
    assert_eq!(
        (
            at_limit[0].old_start,
            at_limit[0].old_lines,
            at_limit[0].new_start,
            at_limit[0].new_lines
        ),
        (1, 0, 1, 10_000)
    );
    assert_eq!(
        structured_patch("", &lines(10_001, |n| n.to_string()), 4),
        None
    );
}

/// 公共前后缀：后缀只在前缀之后剩下的部分里数，两段不重叠。
#[test]
fn common_ends_do_not_overlap() {
    assert_eq!(common_ends(&["a", "b", "c"], &["a", "x", "c"]), (1, 1));
    assert_eq!(common_ends(&["a", "b"], &["a", "b"]), (2, 0));
    assert_eq!(common_ends(&["a", "a"], &["a"]), (1, 0));
    assert_eq!(common_ends(&["x", "a"], &["y", "z", "a"]), (0, 1));
    assert_eq!(common_ends::<&str>(&[], &["a"]), (0, 0));
}
