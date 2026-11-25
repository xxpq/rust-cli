//! 一次判决看到的规则与查找（docs/specs/rust-cli/shell-permissions.md §4.2）。规则串的语法（解析、写回、工具名的
//! 匹配）在 kernel-api 的 `permission::syntax`，与 hooks 的 `if` 条件共用（F125）。

use rust_cli_kernel_api::permission::syntax::{format_rule, tool_matches};
use rust_cli_kernel_api::permission::{
    PermissionRule, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};

/// 一次判决看到的规则：配置快照（含“总是允许”落库后的 LocalSettings 层）、会话级的规则层（调用上下文带来的，来源都是
/// Session，docs/specs/rust-cli/permission-updates.md §3），加上只对这一次判决生效的允许规则（skill 的 `allowed-tools`）。
pub struct Rules<'a> {
    pub base: &'a PermissionRules,
    pub layer: &'a [SourcedRule],
    pub allowed: &'a [PermissionRule],
    /// 自动语义生效时去掉危险的 allow 规则（auto-classifier.md §16.4）；None 时全部生效。
    pub stripped: Option<Stripped>,
}

/// 自动语义生效的那一次判决怎么去掉 allow 规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stripped {
    /// `classifyAllShell`：全部 Bash 的 allow 规则都去掉。
    pub all_shell: bool,
}

/// 找到的规则与它的来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub rule: &'a PermissionRule,
    pub source: RuleSource,
}

impl Found<'_> {
    pub fn text(&self) -> String {
        format_rule(self.rule)
    }
}

impl<'a> Rules<'a> {
    fn all(&self) -> impl Iterator<Item = (RuleBehavior, Found<'a>)> + '_ {
        let sourced = |SourcedRule {
                           source,
                           behavior,
                           rule,
                       }: &'a SourcedRule| {
            (
                *behavior,
                Found {
                    rule,
                    source: *source,
                },
            )
        };
        let allowed = self.allowed.iter().map(|rule| {
            (
                RuleBehavior::Allow,
                Found {
                    rule,
                    source: RuleSource::Session,
                },
            )
        });
        let stripped = self.stripped;
        self.base
            .rules
            .iter()
            .map(sourced)
            .chain(self.layer.iter().map(sourced))
            .chain(allowed)
            .filter(move |(behavior, found)| {
                *behavior != RuleBehavior::Allow
                    || stripped.is_none_or(|stripped| {
                        !crate::auto_rules::dangerous(found.rule, stripped.all_shell)
                    })
            })
    }

    /// 针对整个工具的规则（没有 content）：先查快照，再查会话层，最后查这一次判决的允许规则（[`Self::all`] 的顺序；快照里的
    /// 查找同工具清单的 kernel-api `PermissionRules::whole_tool`）。
    pub fn whole_tool(&self, tool: &str, behavior: RuleBehavior) -> Option<Found<'a>> {
        self.all()
            .find(|(kind, found)| {
                *kind == behavior
                    && found.rule.content.is_none()
                    && tool_matches(&found.rule.tool, tool)
            })
            .map(|(_, found)| found)
    }

    /// 用户的 deny 规则（快照与会话层，规则串，按出现顺序去重）：交给自动模式的分类器列进提示（auto-classifier.md §16.7）。
    pub fn deny_texts(&self) -> Vec<String> {
        let mut texts: Vec<String> = Vec::new();
        for (behavior, found) in self.all() {
            let text = found.text();
            if behavior == RuleBehavior::Deny && !texts.contains(&text) {
                texts.push(text);
            }
        }
        texts
    }

    /// 某个工具名下带 content 的规则。
    pub fn contents(&self, tool: &str, behavior: RuleBehavior) -> Vec<(&'a str, Found<'a>)> {
        self.all()
            .filter(|(kind, found)| *kind == behavior && found.rule.tool == tool)
            .filter_map(|(_, found)| {
                found
                    .rule
                    .content
                    .as_deref()
                    .map(|content| (content, found))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
