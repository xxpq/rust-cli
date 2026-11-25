//! Markdown 文件开头的 frontmatter（agent 定义 agents.md §3、SKILL.md 与命令 ext.md §2.2、规则文件 context.md §11.4）。
//!
//! 步骤（ext.md §2.2）：去掉开头的 BOM，按 [`fence`] 取出开头的 `---` 块与之后的正文；块里的 YAML 由 saphyr 解析（YAML 1.2
//! 的核心 schema，重复的键取后一个，`<<` 合并）。解析失败时给含特殊字符、没有引号的值加上引号，把行首的 tab 换成两个空格，
//! 再解析一次；还失败、或结果不是对象时没有字段。值按 JS 的类型保存（[`Value`]），每个调用方按自己的规则读字段。
//!
//! 以前（工作线 F132）这里是自写的 YAML 子集，规则文件的 `paths` 另有一套模拟 YAML 失败的写法；LIB-SMALL 起三处共用这一份
//! （README §3.2.1）。
//!
//! 几处调用方共用的字段读法也在这里：真假（[`Value::flag`]）、工具清单（[`tool_list`]）、
//! `paths` 的切分与花括号展开（[`paths`]）。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;
use saphyr::{ScalarOwned, YamlLoader, YamlOwned};
use saphyr_parser::{Parser, ScanError};

use crate::js;
use crate::secret::{FALSE_WORDS, TRUE_WORDS};

/// 一个值（JS 的值：YAML 的标量、序列与映射解析之后的样子）。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// JS 的数：整数与小数都是双精度。
    Number(f64),
    String(String),
    List(Vec<Value>),
    /// 键是 JS 对象的键（非字符串的键按 [`Value::js_string`] 换成字符串）。
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// 字符串（JS 的 `typeof v === "string"`）。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    /// 标量的文字：字符串原样，数与布尔按 JS 的 `String(v)`；`null`、列表、对象没有。
    pub fn text(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::String(text) => Some(Cow::Borrowed(text)),
            Self::Bool(_) | Self::Number(_) => Some(Cow::Owned(self.js_string())),
            Self::Null | Self::List(_) | Self::Map(_) => None,
        }
    }

    /// 真假：布尔原样；字符串与数按 `String(v)` 去首尾空白、不分大小写，是 `1` / `true` / `yes` / `on`
    /// 为真、`0` / `false` / `no` / `off` 为假；别的文字、`null`、列表、对象认不出（None）。
    pub fn flag(&self) -> Option<bool> {
        let word = match self {
            Self::Bool(value) => return Some(*value),
            Self::String(_) | Self::Number(_) => self.js_string(),
            Self::Null | Self::List(_) | Self::Map(_) => return None,
        };
        let word = js::string::trim(&word);
        let is = |words: &[&str]| words.iter().any(|known| word.eq_ignore_ascii_case(known));
        if is(&TRUE_WORDS) {
            Some(true)
        } else if is(&FALSE_WORDS) {
            Some(false)
        } else {
            None
        }
    }

    /// JS 的 `String(v)`：数组按 `,` 连接（`null` 项是空串），对象是 `[object Object]`。
    pub fn js_string(&self) -> String {
        match self {
            Self::Null => "null".to_owned(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) if value.is_nan() => "NaN".to_owned(),
            Self::Number(value) if value.is_infinite() => if value.is_sign_positive() {
                "Infinity"
            } else {
                "-Infinity"
            }
            .to_owned(),
            Self::Number(value) => js::json::js_double(*value),
            Self::String(text) => text.clone(),
            Self::List(items) => items
                .iter()
                .map(|item| match item {
                    Self::Null => String::new(),
                    other => other.js_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
            Self::Map(_) => "[object Object]".to_owned(),
        }
    }
}

/// 一个文件的 frontmatter 与正文。
#[derive(Debug, Clone, PartialEq)]
pub struct Frontmatter {
    /// 顶层的字段；没有 frontmatter、YAML 解析失败或顶层不是对象时为空。
    pub fields: BTreeMap<String, Value>,
    /// frontmatter 之后的正文；没有 frontmatter 时是整个原文（带着 BOM）。
    pub body: String,
    /// 加引号重试之后仍解析失败时的原因（调用方记日志；字段为空）。
    pub error: Option<String>,
}

/// JS 正则 `\s` 的字符（[`js::string::is_space`] 的同一组），写成正则的字符类内容。
const SPACE: &str = r"\t\n\x0B\x0C\r \x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}";

/// 固定的正则只编译一次（编不出时为 None，按不匹配处理；单测覆盖每一条）。
fn regex(
    cell: &'static OnceLock<Option<Regex>>,
    pattern: impl FnOnce() -> String,
) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(&pattern()).ok()).as_ref()
}

/// frontmatter 块：`^---\s*\n([\s\S]*?)---\s*\n?`（`\s` 是 JS 的）。结束的 `---` 不必单独成行：值里的 `---` 也结束这一块。
fn fence() -> Option<&'static Regex> {
    static FENCE: OnceLock<Option<Regex>> = OnceLock::new();
    regex(&FENCE, || {
        format!(r"\A---[{SPACE}]*\n((?s:.*?))---[{SPACE}]*\n?")
    })
}

/// 重试时检查的一行：`key: value`，键只有字母、`_`、`-`，值不以空白开头（`.` 不含行结束符，所以 CRLF 的行不算）。
fn simple_line() -> Option<&'static Regex> {
    static LINE: OnceLock<Option<Regex>> = OnceLock::new();
    regex(&LINE, || {
        format!(r"\A([a-zA-Z_-]+):[{SPACE}]+([^{SPACE}][^\n\r\x{{2028}}\x{{2029}}]*)\z")
    })
}

/// 没有引号时要加引号的值：含 YAML 的指示符，或含 `: `（`12:34`、`https://` 不算）。
fn special() -> Option<&'static Regex> {
    static SPECIAL: OnceLock<Option<Regex>> = OnceLock::new();
    regex(&SPECIAL, || r"[{}\[\]*&#!|>%@`]|: ".to_owned())
}

/// 行首的 tab（YAML 的缩进不许用 tab）。
fn leading_tabs() -> Option<&'static Regex> {
    static TABS: OnceLock<Option<Regex>> = OnceLock::new();
    regex(&TABS, || r"(?mR)^\t+".to_owned())
}

/// 解析一个文件开头的 frontmatter。
pub fn parse(raw: &str) -> Frontmatter {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let Some(found) = fence().and_then(|fence| fence.captures(text)) else {
        return Frontmatter {
            fields: BTreeMap::new(),
            body: raw.to_owned(),
            error: None,
        };
    };
    let (whole, block) = (&found[0], &found[1]);
    let body = text[whole.len()..].to_owned();
    let (fields, error) = match load(block).or_else(|_| load(&retry_text(block))) {
        Ok(Value::Map(fields)) => (fields, None),
        Ok(_) => (BTreeMap::new(), None),
        Err(error) => (BTreeMap::new(), Some(error)),
    };
    Frontmatter {
        fields,
        body,
        error,
    }
}

/// 重试用的文字：给 [`simple_line`] 里含特殊字符、没有引号、也不是 YAML 列表的值加双引号（转义 `\` 与 `"`），再把行首的
/// tab 换成两个空格。
fn retry_text(block: &str) -> String {
    let lines: Vec<Cow<'_, str>> = block
        .split('\n')
        .map(|line| {
            let Some(found) = simple_line().and_then(|simple| simple.captures(line)) else {
                return Cow::Borrowed(line);
            };
            let (key, value) = (&found[1], &found[2]);
            let quoted = (value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\''));
            let list = value.starts_with('[')
                && value.ends_with(']')
                && matches!(load(value), Ok(Value::List(_)));
            if quoted || list || !special().is_some_and(|special| special.is_match(value)) {
                return Cow::Borrowed(line);
            }
            let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
            Cow::Owned(format!("{key}: \"{escaped}\""))
        })
        .collect();
    let joined = lines.join("\n");
    match leading_tabs() {
        Some(tabs) => tabs
            .replace_all(&joined, |found: &regex::Captures<'_>| {
                "  ".repeat(found[0].len())
            })
            .into_owned(),
        None => joined,
    }
}

/// 解析 YAML；冒号后紧跟 tab 的值当作冒号后是空格（ext.md §2.2 第 2 步）。
///
/// saphyr 照 YAML 测试集拒绝 `key:<tab>value`（tab 之后是字母、数字或 `-`），frontmatter 要读出这种写法。报错的位置前面是一段
/// tab、再前面是 `:` 时，把这段 tab 换成一个空格再解析。只改扫描器认作键后冒号、又在那里报了错的一处，块文本与引号里的 tab
/// 不动；每次少一处冒号后的 tab，循环一定结束。
fn load(text: &str) -> Result<Value, String> {
    let mut text = Cow::Borrowed(text);
    loop {
        let error = match load_once(&text) {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Some(tabs) = tabs_after_colon(&text, error.marker().index()) else {
            return Err(error.to_string());
        };
        text = Cow::Owned(format!("{} {}", &text[..tabs.start], &text[tabs.end..]));
    }
}

/// 字符下标 `index`（saphyr 的位置按字符数）前面紧挨着、又跟在 `:` 后面的一段 tab 的字节范围。
fn tabs_after_colon(text: &str, index: usize) -> Option<Range<usize>> {
    let end = text
        .char_indices()
        .nth(index)
        .map_or(text.len(), |(at, _)| at);
    let start = text[..end].trim_end_matches('\t').len();
    (start < end && text[..start].ends_with(':')).then_some(start..end)
}

/// 解析 YAML。重复的键取后一个；没有文档时是 `null`，多个文档时是它们的列表（都不是对象）。
fn load_once(text: &str) -> Result<Value, ScanError> {
    let mut loader = YamlLoader::<YamlOwned>::default();
    loader.allow_duplicate_keys(true);
    Parser::new_from_str(text).load(&mut loader, true)?;
    if let Some(error) = loader.error() {
        return Err(error.clone());
    }
    let mut documents = loader.into_documents();
    Ok(match documents.len() {
        0 => Value::Null,
        1 => convert(documents.remove(0)),
        _ => Value::List(documents.into_iter().map(convert).collect()),
    })
}

fn convert(node: YamlOwned) -> Value {
    match node {
        YamlOwned::Value(scalar) => match scalar {
            ScalarOwned::Null => Value::Null,
            ScalarOwned::Boolean(value) => Value::Bool(value),
            #[expect(
                clippy::cast_precision_loss,
                reason = "JS 只有双精度：超过 2^53 的整数照 JS 损失精度"
            )]
            ScalarOwned::Integer(value) => Value::Number(value as f64),
            ScalarOwned::FloatingPoint(value) => Value::Number(value.into_inner()),
            ScalarOwned::String(text) => Value::String(text),
        },
        YamlOwned::Representation(text, _, _) => Value::String(text),
        YamlOwned::Sequence(items) => Value::List(items.into_iter().map(convert).collect()),
        YamlOwned::Mapping(entries) => {
            let mut fields = BTreeMap::new();
            let mut merged = Vec::new();
            for (key, value) in entries {
                let key = convert(key).js_string();
                let value = convert(value);
                // `<<` 合并另一个映射（或映射的列表）：本映射里写了的键优先。
                match (key.as_str(), value) {
                    (MERGE_KEY, Value::Map(map)) => merged.push(map),
                    (MERGE_KEY, Value::List(items)) => {
                        merged.extend(items.into_iter().filter_map(|item| match item {
                            Value::Map(map) => Some(map),
                            _ => None,
                        }));
                    }
                    (_, value) => {
                        fields.insert(key, value);
                    }
                }
            }
            for map in merged {
                for (key, value) in map {
                    fields.entry(key).or_insert(value);
                }
            }
            Value::Map(fields)
        }
        // 自定义的标签不改变值。
        YamlOwned::Tagged(_, node) => convert(*node),
        // 解析器已经展开了别名；坏值（找不到的别名）按 null。
        YamlOwned::Alias(_) | YamlOwned::BadValue => Value::Null,
    }
}

/// YAML 的合并键。
const MERGE_KEY: &str = "<<";

/// 工具清单：字符串，或列表里的字符串项；`null`、布尔、数、对象是空清单。每一项按括号外的逗号与
/// 空格切分（只认空格，别的空白留在项里；括号是开关，不计层数），去首尾空白，空项去掉；含 `*` 时是 `["*"]`。
/// skill 的 `allowed-tools` / `disallowed-tools` 与 agent 的 `tools` / `disallowedTools` 都这样读（ext.md §2.2、agents.md §3）。
pub fn tool_list(value: &Value) -> Vec<String> {
    let items: Vec<&str> = match value {
        Value::String(text) => vec![text],
        Value::List(items) => items.iter().filter_map(Value::as_str).collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Map(_) => Vec::new(),
    };
    let mut names = Vec::new();
    let mut flush = |current: &mut String| {
        let name = js::string::trim(current);
        if !name.is_empty() {
            names.push(name.to_owned());
        }
        current.clear();
    };
    for item in items {
        let mut current = String::new();
        let mut in_parens = false;
        for c in item.chars() {
            match c {
                '(' | ')' => {
                    in_parens = c == '(';
                    current.push(c);
                }
                ',' | ' ' if !in_parens => flush(&mut current),
                // 项开头的空白不要：只在已有文字或不是空白时追加。
                _ if current.is_empty() && js::string::is_space(c) => {}
                _ => current.push(c),
            }
        }
        flush(&mut current);
    }
    if names.iter().any(|name| name == ALL_TOOLS) {
        return vec![ALL_TOOLS.to_owned()];
    }
    names
}

/// 工具清单里表示“全部工具”的项。
pub const ALL_TOOLS: &str = "*";

/// `paths` 的花括号展开最多给出的项数（一个 `paths` 共用）。
const MAX_EXPANDED_PATTERNS: usize = 1000;
/// 花括号展开最多用掉的 UTF-16 码元数（同上）。
const MAX_EXPANDED_UNITS: usize = 4_194_304;

/// 条件 skill 与条件规则文件的 `paths`（ext.md §2.2、context.md §11.4）：字符串按
/// 花括号外的逗号切分、去首尾空白、展开花括号；列表逐项（可以嵌套），只取字符串。之后去掉结尾的 `/**` 与空项；没有项或全是
/// `**` 时为 None（无条件）。
pub fn paths(value: Option<&Value>) -> Option<Vec<String>> {
    let mut budget = Budget {
        patterns: MAX_EXPANDED_PATTERNS,
        units: MAX_EXPANDED_UNITS,
    };
    let mut patterns = Vec::new();
    collect_paths(value?, &mut budget, &mut patterns);
    let patterns: Vec<String> = patterns
        .into_iter()
        .map(|pattern| match pattern.strip_suffix("/**") {
            Some(stripped) => stripped.to_owned(),
            None => pattern,
        })
        .filter(|pattern| !pattern.is_empty())
        .collect();
    (!patterns.is_empty() && !patterns.iter().all(|pattern| pattern == "**")).then_some(patterns)
}

/// 花括号展开还剩的预算：扣掉这一项之后不够就算超出，之后的展开都超出（扣到 0 为止）。
struct Budget {
    patterns: usize,
    units: usize,
}

fn collect_paths(value: &Value, budget: &mut Budget, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            for part in split_top_level_commas(text) {
                out.extend(expand_braces(part, budget));
            }
        }
        Value::List(items) => items
            .iter()
            .for_each(|item| collect_paths(item, budget, out)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Map(_) => {}
    }
}

/// 花括号外的逗号切分（`{` 加一层、`}` 减一层，可以减到负数），每段去首尾空白，空段去掉。
fn split_top_level_commas(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (at, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
        .into_iter()
        .map(js::string::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// 展开花括号：每次取第一个 `{…}`（里面至少一个字符、没有 `}`，`}` 之后不能有换行），按逗号分开、去首尾空白，
/// 依次替换，深度优先。超出预算时这一项原样返回，已经扣掉的不退回。
fn expand_braces(pattern: &str, budget: &mut Budget) -> Vec<String> {
    if !pattern.contains('{') {
        return vec![pattern.to_owned()];
    }
    let units = js::string::utf16_len(pattern);
    let mut done = Vec::new();
    let mut pending = vec![pattern.to_owned()];
    while let Some(item) = pending.pop() {
        let Some((prefix, inner, suffix)) = first_brace_group(&item) else {
            done.push(item);
            continue;
        };
        let item_units = js::string::utf16_len(&item);
        let alternatives: Vec<&str> = inner.split(',').map(js::string::trim).collect();
        let count = done.len() + pending.len() + alternatives.len();
        let over = item_units > budget.units
            || count > budget.patterns
            || count.saturating_mul(units) > budget.units - item_units;
        budget.units = budget.units.saturating_sub(item_units);
        if over {
            return vec![pattern.to_owned()];
        }
        pending.extend(
            alternatives
                .iter()
                .rev()
                .map(|alternative| format!("{prefix}{alternative}{suffix}")),
        );
    }
    budget.patterns = budget.patterns.saturating_sub(done.len());
    budget.units = budget
        .units
        .saturating_sub(done.len().saturating_mul(units));
    done
}

/// 第一个 `{` 之前、它到第一个 `}` 之间、之后的三段；中间为空、或 `}` 之后有换行时没有。
fn first_brace_group(item: &str) -> Option<(&str, &str, &str)> {
    let open = item.find('{')?;
    let close = open + 1 + item[open + 1..].find('}')?;
    let (inner, suffix) = (&item[open + 1..close], &item[close + 1..]);
    let line_break = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    (!inner.is_empty() && !suffix.contains(line_break)).then_some((&item[..open], inner, suffix))
}

#[cfg(test)]
#[path = "frontmatter_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "frontmatter_parity_tests.rs"]
mod parity_tests;
