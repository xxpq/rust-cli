//! 规则文件的 frontmatter：只取条件规则的 `paths`（context.md §11.4）。
//!
//! frontmatter 由 base 解析（与 skill、agent 文件同一份；LIB-SMALL 以前这里另有一套只认 `paths` 的 YAML 模拟，
//! README §3.2.1）。`paths` 的切分与花括号展开也在 base（[`frontmatter::paths`]，与条件 skill 共用；以前在这里）。

use rust_cli_base::frontmatter;

/// frontmatter 之后的正文，与 `paths` 给出的 glob（None 表示无条件规则）。
pub(crate) fn split(raw: &str) -> (String, Option<Vec<String>>) {
    let parsed = frontmatter::parse(raw);
    if let Some(error) = &parsed.error {
        tracing::warn!(%error, "the YAML frontmatter of a rules file failed to parse and was ignored");
    }
    let globs = frontmatter::paths(parsed.fields.get("paths"));
    (parsed.body, globs)
}

#[cfg(test)]
#[path = "rules_frontmatter_tests.rs"]
mod tests;
