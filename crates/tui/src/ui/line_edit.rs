//! 单行输入的编辑（架构整理 U12，用户决定 Q8，docs/specs/rust-cli/tui-input.md §5）：只在末尾增删（与原来一样），
//! backspace 删掉最后一个字素，粘贴时换行压成空格。
//!
//! 缺陷修复：以前 15 个单行输入各写 `String::pop`，组合字符（`e` 加组合的锐音符）与带肤色的表情只删掉一半；列表的过滤、
//! 详细视图与 ← 项目页的搜索、输入历史的搜索、`/permissions` 的过滤不收粘贴（丢掉，或进了被盖住的输入框）。
//!
//! 词的边界（[`word_start`]、[`word_end`]）所有文字输入共用一份：输入框、设置页与单行输入（tui-input.md §5「按词删除与
//! 移动」）。以前设置页按空白分词，输入框按字母与数字分，同一个键在两处删掉的不一样。
use unicode_segmentation::UnicodeSegmentation;

/// 删掉最后一个字素；空的时候返回 false。
pub fn backspace(text: &mut String) -> bool {
    match text.grapheme_indices(true).next_back() {
        Some((at, _)) => {
            text.truncate(at);
            true
        }
        None => false,
    }
}

/// 删掉末尾的一个词（先删掉末尾的分隔）；空的时候返回 false。
pub fn kill_word(text: &mut String) -> bool {
    let start = word_start(text, text.len());
    let changed = start < text.len();
    text.truncate(start);
    changed
}

/// 在末尾粘贴：换行（`\r\n`、`\n`、`\r`）压成一个空格。
pub fn paste(text: &mut String, pasted: &str) {
    text.push_str(&pasted.replace("\r\n", " ").replace(['\n', '\r'], " "));
}

/// 词里的字素：第一个字符是字母或数字（组合的重音符随前面的字母算进词里）。
fn is_word(grapheme: &str) -> bool {
    grapheme.chars().next().is_some_and(char::is_alphanumeric)
}

/// `at`（字节下标，在字符边界上）前的上一个词首：先跳过分隔，再跳过词。字母与数字连成的一段是一个词，别的都是分隔。
///
/// 按字素走：以前按字符判断，`e` 加组合的锐音符时词尾停在两者之间，删后一个词留下半个字素（设置页的光标要求在字素
/// 边界上）。
pub fn word_start(text: &str, at: usize) -> usize {
    let mut start = at;
    let mut in_word = false;
    for (i, g) in text[..at].grapheme_indices(true).rev() {
        if is_word(g) {
            in_word = true;
        } else if in_word {
            break;
        }
        start = i;
    }
    start
}

/// `at` 后的下一个词尾：先跳过分隔，再跳过词。
pub fn word_end(text: &str, at: usize) -> usize {
    let mut end = at;
    let mut in_word = false;
    for (i, g) in text[at..].grapheme_indices(true) {
        if is_word(g) {
            in_word = true;
        } else if in_word {
            break;
        }
        end = at + i + g.len();
    }
    end
}

#[cfg(test)]
#[path = "line_edit_tests.rs"]
mod tests;
