//! agent 文件的 frontmatter（agents.md §3）：解析器在 base（与 SKILL.md 共用，F132 的 T8；LIB-SMALL 起是真的 YAML），
//! 这里只有 agent 专用的读法。`null` 与嵌套的对象不是文字：[`Value::text`] 不给它们；`tools` 写了却不是清单时是空清单。

use std::collections::BTreeMap;

use rust_cli_base::frontmatter::{ALL_TOOLS, tool_list};
pub(crate) use rust_cli_base::frontmatter::{Frontmatter, Value, parse};

/// 标量字段去掉首尾空白之后的文字；空的按没写。
pub(crate) fn text(fields: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    let value = fields.get(key)?.text()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// 工具清单（agents.md §3）：没写是全部（None）；写了按 base 的 [`tool_list`] 读，空着（`null`）与不是字符串或列表的值是空清单
/// （没有工具）；含 `*` 是全部。修复：以前空着与对象按没写（全部工具），清单只按逗号切（`Read Grep` 是一个名字）。
pub(crate) fn tools(value: Option<&Value>) -> Option<Vec<String>> {
    let names = tool_list(value?);
    (!names.iter().any(|name| name == ALL_TOOLS)).then_some(names)
}

/// 解析一个 agent 文件；YAML 解析失败时记一行日志（字段为空，与参照相同）。
pub(crate) fn read(raw: &str, path: &str) -> Frontmatter {
    let parsed = parse(raw);
    if let Some(error) = &parsed.error {
        tracing::warn!(path, %error, "the YAML frontmatter failed to parse and was ignored");
    }
    parsed
}

#[cfg(test)]
#[path = "frontmatter_tests.rs"]
mod tests;
