//! hook 运行事实里的文字（docs/specs/rust-cli/hooks.md §14.2）：显示用的命令、预览的截断与脱敏。
//!
//! 脱敏逐条照 v4 的 `sanitizeHookDisplayText`（出处在 spec §14.1）：hook 的命令、拦下的原因与 stderr 可能带凭据，离开引擎之前处理一次。正则是 ECMAScript 语义（`regress`，与 matcher 同一个引擎），所以与 v4 逐字相同。

use std::sync::LazyLock;

use rust_cli_base::js::string;

use crate::config::HookCommand;

/// 换掉凭据的写法（v4 的 `MASK`）。
const MASK: &str = "••••";
/// 预览最多留的 UTF-16 码元数（v4 的 `trimForPreview` 与 `sanitizeHookDiagnostics` 的 4000）。
const PREVIEW_UNITS: usize = 4000;
/// 截断之后接的记号（v4 的 `trimForPreview`）。
const TRUNCATED: &str = "...";

/// 名字像凭据的键（v4 的 `SENSITIVE_KEY` 与 `SENSITIVE_IDENTIFIER`）。
const SENSITIVE_KEY: &str =
    "(?:access[_-]?token|api[_-]?key|credential|password|private[_-]?key|secret|token)";

fn sensitive_identifier() -> String {
    format!("(?:[A-Za-z0-9]+[_-])*{SENSITIVE_KEY}(?:[_-][A-Za-z0-9]+)*")
}

/// 替换的一段：第几个捕获组，或一段文字。
#[derive(Clone, Copy)]
enum Part {
    Group(usize),
    Text(&'static str),
}

/// 换掉凭据的那一段。
const MASKED: Part = Part::Text(MASK);

struct Rule {
    regex: regress::Regex,
    replacement: &'static [Part],
}

fn rule(pattern: &str, flags: &str, replacement: &'static [Part]) -> Rule {
    #[expect(
        clippy::expect_used,
        reason = "固定的正则：模式是上面的常量，单测逐条覆盖"
    )]
    let regex = regress::Regex::with_flags(pattern, flags).expect("hook sanitize pattern");
    Rule { regex, replacement }
}

/// v4 的 7 条替换，按顺序。
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    use Part::{Group, Text};
    let id = sensitive_identifier();
    vec![
        rule(
            r"([a-z][a-z0-9+.-]*:\/\/)[^\s/:@]+:[^\s/@]+@",
            "iu",
            &[Group(1), MASKED, Text(":"), MASKED, Text("@")],
        ),
        rule(
            r#"(authorization\s*:\s*(?:basic|bearer)\s+)(?:"[^"]*"|'[^']*'|[^\s,;"']+)"#,
            "iu",
            &[Group(1), MASKED],
        ),
        rule(
            &format!(r#"([?&](?:authorization|{id})=)[^&\s"']+"#),
            "iu",
            &[Group(1), MASKED],
        ),
        rule(
            r#"(\bauthorization\b\s*=\s*)(?:"[^"]*"|'[^']*'|[^\s,"']+)"#,
            "iu",
            &[Group(1), MASKED],
        ),
        rule(
            &format!(r#"((?:"{id}"|'{id}')\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,"']+)"#),
            "iu",
            &[Group(1), MASKED],
        ),
        rule(
            &format!(r#"(^|[^A-Za-z0-9_])({id}\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,"']+)"#),
            "imu",
            &[Group(1), Group(2), MASKED],
        ),
        rule(
            &format!(r#"((?:--)?{id})(\s+)(?:"[^"]*"|'[^']*'|[^\s]+)"#),
            "iu",
            &[Group(1), Group(2), MASKED],
        ),
    ]
});

/// 一条替换（JS `replace` 带 `g`）：每个匹配换成 `replacement` 拼出的文字。
fn replace(text: &str, rule: &Rule) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in rule.regex.find_iter(text) {
        let range = found.range();
        out.push_str(&text[last..range.start]);
        for part in rule.replacement {
            match part {
                Part::Group(index) => {
                    if let Some(group) = found.group(*index) {
                        out.push_str(&text[group]);
                    }
                }
                Part::Text(text) => out.push_str(text),
            }
        }
        last = range.end;
    }
    out.push_str(&text[last..]);
    out
}

/// v4 的 `sanitizeHookDisplayText`。
pub(crate) fn sanitize(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    RULES
        .iter()
        .fold(text.to_owned(), |text, rule| replace(&text, rule))
}

/// 输出的预览（v4 的 `trimForPreview`）：去掉首尾空白，为空时没有；超过 4000 码元截断加 `...`。还没脱敏。
pub(crate) fn cut(text: &str) -> Option<String> {
    let trimmed = string::trim(text);
    if trimmed.is_empty() {
        return None;
    }
    Some(if string::utf16_len(trimmed) > PREVIEW_UNITS {
        format!(
            "{}{TRUNCATED}",
            string::utf16_prefix(trimmed, PREVIEW_UNITS)
        )
    } else {
        trimmed.to_owned()
    })
}

/// 离开引擎的文字（stderr 的预览、拦下的原因）：脱敏，再截到 4000 码元（v4 的 `sanitizeHookDiagnostics`；原因 v4 不截，这里同样截住，
/// hooks.md §14.2）。
pub(crate) fn shown(text: &str) -> String {
    string::utf16_prefix(&sanitize(text), PREVIEW_UNITS).to_owned()
}

/// v4 的 `quoteCommandPart`：只含安全字符的原样，否则写成 JSON 字符串。
fn quote_part(part: &str) -> String {
    let plain = !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:@%+=,-".contains(c));
    if plain {
        part.to_owned()
    } else {
        serde_json::Value::from(part).to_string()
    }
}

/// 显示用的命令（v4 的 `commandDisplay`）：exec 形式每一项照 `quoteCommandPart` 写、用空格接起；shell 形式是命令原文；prompt hook 是
/// prompt，http hook 是 URL。不展开 `${…}` 变量（行只用它取可执行文件的名字）。脱敏。
pub(crate) fn command(hook: &HookCommand) -> String {
    let shown = match hook {
        HookCommand::Command(command) => match &command.args {
            Some(args) => std::iter::once(&command.command)
                .chain(args)
                .map(|part| quote_part(part))
                .collect::<Vec<_>>()
                .join(" "),
            None => command.command.clone(),
        },
        HookCommand::Prompt(_) | HookCommand::Agent(_) | HookCommand::Http(_) => hook.display(),
    };
    sanitize(&shown)
}

#[cfg(test)]
#[path = "display_tests.rs"]
mod tests;
