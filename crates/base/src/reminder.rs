//! `<system-reminder>` 包装。attachment 落库的文本带包装；折叠进 system 消息时去掉。
//!
//! F58：唯一定义从 prompts 移到这里，因为 model 与 node-store 不依赖 prompts；以前 tools、prompts 的子代理通知、
//! node-store、model 的两种协议、query 的命令通知与 kernel-api 的测试替身各手写一份。prompts 的 `reminder` 重新导出。

use crate::chars;

pub const OPEN: &str = "<system-reminder>";
pub const CLOSE: &str = "</system-reminder>";

/// `<system-reminder>\n{text}\n</system-reminder>`。
pub fn wrap(text: &str) -> String {
    format!("{OPEN}\n{text}\n{CLOSE}")
}

/// 去掉一层包装；开头的换行与结尾前的换行各去一个。不是完整包装的文本原样返回。
pub fn unwrap(text: &str) -> &str {
    let Some(inner) = text
        .strip_prefix(OPEN)
        .and_then(|rest| rest.strip_suffix(CLOSE))
    else {
        return text;
    };
    let inner = inner.strip_prefix('\n').unwrap_or(inner);
    inner.strip_suffix('\n').unwrap_or(inner)
}

pub fn is_wrapped(text: &str) -> bool {
    text.starts_with(OPEN)
}

/// DEL（U+007F）。
const DEL: char = '\u{7f}';

/// 外部值（路径等）放进提醒的文字之前的转义：尖括号换成实体，C0 控制字符与 DEL 换成 `&#N;`。环境段与前端层写的路径
/// 共用这一份（以前在 context 的 `render`）。
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // C0 的范围只定义在 `chars`；这里另加 DEL，不含 C1。
            c if chars::is_c0(c) || c == DEL => {
                out.push_str(&format!("&#{};", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_is_byte_for_byte_and_unwrap_reverses_it() {
        let wrapped = wrap("note\nmore");
        assert_eq!(wrapped, "<system-reminder>\nnote\nmore\n</system-reminder>");
        assert!(is_wrapped(&wrapped));
        assert_eq!(unwrap(&wrapped), "note\nmore");
        assert_eq!(unwrap("plain"), "plain");
        assert_eq!(wrap(""), "<system-reminder>\n\n</system-reminder>");
    }

    #[test]
    fn escaping_replaces_angle_brackets_and_control_characters() {
        assert_eq!(
            escape("a<b>\u{1}\u{7f}\u{85}é"),
            "a&lt;b&gt;&#1;&#127;\u{85}é"
        );
    }
}
