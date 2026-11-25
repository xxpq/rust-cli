//! 自动语义生效时不生效的 allow 规则（docs/specs/rust-cli/auto-classifier.md §16.4）：能直接执行任意代码、会绕过分类器的那些。
//! 规则快照不改：自动语义生效的那一次判决读 allow 规则时跳过它们。

use std::sync::LazyLock;

use regex::Regex;
use rust_cli_kernel_api::permission::PermissionRule;
use rust_cli_kernel_api::tool_names;

/// Bash 规则里算作解释器的前缀（两个词的是整体）。
const INTERPRETERS: [&str; 26] = [
    "python", "python3", "python2", "node", "deno", "tsx", "ruby", "perl", "php", "lua", "npx",
    "bunx", "npm run", "yarn run", "pnpm run", "bun run", "bash", "sh", "ssh", "zsh", "fish",
    "eval", "exec", "env", "xargs", "sudo",
];

/// `python -m 包.模块` 这一种带选项的前缀不算危险（只跑一个具名的模块）。
static PYTHON_MODULE: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(clippy::expect_used, reason = "固定的正则")]
    Regex::new(r"^-m\s+[A-Za-z0-9_]+\.[A-Za-z0-9_.]+(\s*:|\s+)$")
        .expect("valid python module pattern")
});
static PYTHON: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(clippy::expect_used, reason = "固定的正则")]
    Regex::new(r"^python[0-9.]*$").expect("valid python pattern")
});

/// 这条 allow 规则在自动语义生效时不生效：Bash 的整个工具、解释器前缀与带选项的解释器前缀（`all_shell` 时全部 Bash 规则）；
/// Agent 与 Monitor 的全部规则。
pub(crate) fn dangerous(rule: &PermissionRule, all_shell: bool) -> bool {
    match rule.tool.as_str() {
        tool_names::BASH => all_shell || rule.content.as_deref().is_none_or(dangerous_shell),
        tool_names::AGENT | tool_names::MONITOR => true,
        _ => false,
    }
}

fn dangerous_shell(content: &str) -> bool {
    if content.chars().all(|c| c.is_whitespace() || c == '*') {
        return true;
    }
    let rule = content.trim().to_lowercase();
    INTERPRETERS.iter().any(|name| {
        if [
            (*name).to_owned(),
            format!("{name}:*"),
            format!("{name} *"),
            format!("{name}*"),
        ]
        .contains(&rule)
        {
            return true;
        }
        let Some(rest) = rule
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|rest| rest.strip_suffix('*'))
        else {
            return false;
        };
        rest.starts_with('-') && !(PYTHON.is_match(name) && PYTHON_MODULE.is_match(rest))
    })
}

#[cfg(test)]
#[path = "auto_rules_tests.rs"]
mod tests;
