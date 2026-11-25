//! 行级 diff（docs/specs/rust-cli/tools-files.md §4.1、session-lifecycle.md §10.45）：tools 的改动片段（上下文 8 行）与 session 的
//! 回合文件 diff（上下文 4 行）共用这一份。
//!
//! 算法照 jsdiff 9（`diff@9.0.0` 的 `base.js`、`line.js`、`patch/create.js`）移植：按行切分（行尾换行算进行里），
//! Myers 的对角线逐步推进，同一步里能删能增时先看哪条路径走得更远；hunk 按上下文行数合并，没有末尾换行的那一行之后加一行标记。

use std::rc::Rc;

use crate::text::MAX_DIFF_EDITS;

/// Myers 每一步只走同奇偶的对角线。
const DIAGONAL_STEP: isize = 2;
/// 最后这么多个 change 之外的公共段，短时并进同一个 hunk。
const TAIL_CHANGES: usize = 2;
/// 没有末尾换行的那一行之后的标记行（jsdiff 的原文）。
pub const NO_NEWLINE_MARKER: &str = "\\ No newline at end of file";

/// 一个 hunk（jsdiff `structuredPatch` 的形状）：起始行从 1 数；`lines` 带 ` ` / `+` / `-` 前缀，不带换行，
/// 没有末尾换行的那一行之后是 [`NO_NEWLINE_MARKER`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<String>,
}

/// 一段连续的相同 / 新增 / 删除的行（jsdiff 的 component）。
struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<Rc<Component>>,
}

#[derive(Clone)]
struct Path {
    old_pos: isize,
    last: Option<Rc<Component>>,
}

/// 按行切分：每个元素是一行连同它的换行（`\n` 或 `\r\n`），最后一行可以没有换行；空行之外的空串去掉。
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    for (index, _) in text.match_indices('\n') {
        tokens.push(&text[start..=index]);
        start = index + 1;
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

fn add_to_path(path: &Path, added: bool, removed: bool, old_step: isize) -> Path {
    let last = match &path.last {
        Some(last) if last.added == added && last.removed == removed => Component {
            count: last.count + 1,
            added,
            removed,
            previous: last.previous.clone(),
        },
        last => Component {
            count: 1,
            added,
            removed,
            previous: last.clone(),
        },
    };
    Path {
        old_pos: path.old_pos + old_step,
        last: Some(Rc::new(last)),
    }
}

/// 沿对角线走过相同的行；返回新文件里的位置。
fn extract_common(path: &mut Path, new: &[&str], old: &[&str], diagonal: isize) -> isize {
    let (new_len, old_len) = (new.len() as isize, old.len() as isize);
    let mut old_pos = path.old_pos;
    let mut new_pos = old_pos - diagonal;
    let mut common = 0;
    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && old[(old_pos + 1) as usize] == new[(new_pos + 1) as usize]
    {
        new_pos += 1;
        old_pos += 1;
        common += 1;
    }
    if common > 0 {
        path.last = Some(Rc::new(Component {
            count: common,
            added: false,
            removed: false,
            previous: path.last.take(),
        }));
    }
    path.old_pos = old_pos;
    new_pos
}

/// 一段改动：行（连同换行）与种类。
struct Change<'a> {
    lines: Vec<&'a str>,
    added: bool,
    removed: bool,
}

fn values<'a>(last: Option<Rc<Component>>, new: &[&'a str], old: &[&'a str]) -> Vec<Change<'a>> {
    let mut chain = Vec::new();
    let mut cursor = last;
    while let Some(component) = cursor {
        cursor = component.previous.clone();
        chain.push(component);
    }
    chain.reverse();
    let (mut new_pos, mut old_pos) = (0, 0);
    chain
        .into_iter()
        .map(|component| {
            let lines = if component.removed {
                let lines = old[old_pos..old_pos + component.count].to_vec();
                old_pos += component.count;
                lines
            } else {
                let lines = new[new_pos..new_pos + component.count].to_vec();
                new_pos += component.count;
                if !component.added {
                    old_pos += component.count;
                }
                lines
            };
            Change {
                lines,
                added: component.added,
                removed: component.removed,
            }
        })
        .collect()
}

/// 两段文字的逐行改动；编辑距离超过上限时为 None（相当于 jsdiff 的算 diff 超时）。
fn diff_lines<'a>(old_text: &'a str, new_text: &'a str) -> Option<Vec<Change<'a>>> {
    let old = tokenize(old_text);
    let new = tokenize(new_text);
    let (new_len, old_len) = (new.len() as isize, old.len() as isize);
    let max_edits = (new.len() + old.len()).min(MAX_DIFF_EDITS) as isize;
    let offset = max_edits + 1;
    let slot = |diagonal: isize| (diagonal + offset) as usize;
    let mut best: Vec<Option<Path>> = vec![None; (2 * offset + 1) as usize];
    let mut first = Path {
        old_pos: -1,
        last: None,
    };
    let new_pos = extract_common(&mut first, &new, &old, 0);
    if first.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        return Some(values(first.last, &new, &old));
    }
    best[slot(0)] = Some(first);
    let (mut min_diagonal, mut max_diagonal) = (isize::MIN, isize::MAX);
    for edits in 1..=max_edits {
        let mut diagonal = min_diagonal.max(-edits);
        while diagonal <= max_diagonal.min(edits) {
            let remove = best[slot(diagonal - 1)].take();
            let add = best[slot(diagonal + 1)].clone();
            let can_add = add
                .as_ref()
                .is_some_and(|path| (0..new_len).contains(&(path.old_pos - diagonal)));
            let can_remove = remove
                .as_ref()
                .is_some_and(|path| path.old_pos + 1 < old_len);
            if !can_add && !can_remove {
                best[slot(diagonal)] = None;
                diagonal += DIAGONAL_STEP;
                continue;
            }
            // 删不了，或者能增且增的那条路在旧文件里走得更远：增；否则删。
            let mut path = match (&remove, &add) {
                (Some(remove), Some(add)) if can_add && remove.old_pos < add.old_pos => {
                    add_to_path(add, true, false, 0)
                }
                (Some(remove), _) if can_remove => add_to_path(remove, false, true, 1),
                (_, Some(add)) => add_to_path(add, true, false, 0),
                _ => {
                    best[slot(diagonal)] = None;
                    diagonal += DIAGONAL_STEP;
                    continue;
                }
            };
            let new_pos = extract_common(&mut path, &new, &old, diagonal);
            if path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return Some(values(path.last, &new, &old));
            }
            if path.old_pos + 1 >= old_len {
                max_diagonal = max_diagonal.min(diagonal - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal = min_diagonal.max(diagonal + 1);
            }
            best[slot(diagonal)] = Some(path);
            diagonal += DIAGONAL_STEP;
        }
    }
    None
}

/// 打开着的 hunk：旧、新文件里的起始行与已收的行（带前缀与换行）。
struct Open {
    old_start: usize,
    new_start: usize,
    lines: Vec<String>,
}

/// 一组改动的 hunk（jsdiff `structuredPatch` 的第一步：行还带着换行）。
fn hunks(changes: &[Change<'_>], context: usize) -> Vec<Hunk> {
    let mut out = Vec::new();
    // 末尾补一段空的相同行，最后一个 hunk 由它收尾。
    let tail = Change {
        lines: Vec::new(),
        added: false,
        removed: false,
    };
    let all: Vec<&Change<'_>> = changes.iter().chain(std::iter::once(&tail)).collect();
    let prefixed = |sign: char, lines: &[&str]| -> Vec<String> {
        lines.iter().map(|line| format!("{sign}{line}")).collect()
    };
    let mut open: Option<Open> = None;
    let (mut old_line, mut new_line) = (1_usize, 1_usize);
    for (index, change) in all.iter().enumerate() {
        if change.added || change.removed {
            let hunk = open.get_or_insert_with(|| {
                // 开一个 hunk：前一段相同的行的最后 `context` 行是它的上下文。
                let lead = match index.checked_sub(1).map(|previous| all[previous]) {
                    Some(previous) if context > 0 => {
                        let skip = previous.lines.len().saturating_sub(context);
                        prefixed(' ', &previous.lines[skip..])
                    }
                    _ => Vec::new(),
                };
                Open {
                    old_start: old_line - lead.len(),
                    new_start: new_line - lead.len(),
                    lines: lead,
                }
            });
            let sign = if change.added { '+' } else { '-' };
            hunk.lines.extend(prefixed(sign, &change.lines));
            if change.added {
                new_line += change.lines.len();
            } else {
                old_line += change.lines.len();
            }
            continue;
        }
        if let Some(mut hunk) = open.take() {
            if change.lines.len() <= context * 2 && index + TAIL_CHANGES < all.len() {
                // 相同的行不多：并进这个 hunk，它继续开着
                hunk.lines.extend(prefixed(' ', &change.lines));
                open = Some(hunk);
            } else {
                let trail = change.lines.len().min(context);
                hunk.lines.extend(prefixed(' ', &change.lines[..trail]));
                out.push(Hunk {
                    old_start: hunk.old_start,
                    old_lines: old_line - hunk.old_start + trail,
                    new_start: hunk.new_start,
                    new_lines: new_line - hunk.new_start + trail,
                    lines: hunk.lines,
                });
            }
        }
        old_line += change.lines.len();
        new_line += change.lines.len();
    }
    for hunk in &mut out {
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines.drain(..) {
            match line.strip_suffix('\n') {
                Some(stripped) => lines.push(stripped.to_owned()),
                None => {
                    lines.push(line);
                    lines.push(NO_NEWLINE_MARKER.to_owned());
                }
            }
        }
        hunk.lines = lines;
    }
    out
}

/// jsdiff 的 `structuredPatch(_, _, old, new, _, _, {context})` 的 hunk；编辑距离超过 [`MAX_DIFF_EDITS`] 时为 None。
/// 没有改动时为空。
pub fn structured_patch(old: &str, new: &str, context: usize) -> Option<Vec<Hunk>> {
    diff_lines(old, new).map(|changes| hunks(&changes, context))
}

/// 两列行的公共前缀长，与前缀之后剩下的两段的公共后缀长（两段不重叠）。去掉公共前后缀的简单 diff（不是最小 diff）都从这一步
/// 开始：tools 的 checkpoint 改动片段与 tui 的改动预览（以前各写一份，mature-libraries.md §1.1 A4）。分行与上下文由调用方定。
pub fn common_ends<T: PartialEq>(old: &[T], new: &[T]) -> (usize, usize) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, suffix)
}

#[cfg(test)]
#[path = "line_diff_tests.rs"]
mod tests;
