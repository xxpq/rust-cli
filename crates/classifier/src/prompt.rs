//! 提示与格式（docs/specs/rust-cli/auto-classifier.md §15.4、§15.6.2）：有分数表的模型用分数格式，其余用 `<block>` 格式。用户的
//! `autoMode` 规则换掉模板四个槽的缺省，deny 规则列在一行里（§16.7）。

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use rust_cli_base::models;
use rust_cli_kernel_api::classifier::UserRules;
use rust_cli_kernel_api::permission::auto_mode::AutoModeRules;
use rust_cli_prompts::auto_mode;

use crate::parse::slug;

/// 分类器的输出格式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Format {
    /// `<severity>N</severity>`：第一阶段 `N > t1`、第二阶段 `N > t2` 算拦下。
    Severity { t1: f64, t2: f64 },
    /// `<block>yes|no</block>`。
    Block,
}

/// 分数表（对照实现的缺省配置）：模型（不含 1M 档位的后缀）与两个阈值。
const SEVERITY_TABLE: [(&str, f64, f64); 2] = [
    (models::SONNET_5, 25.0, 35.0),
    (models::OPUS_4_8, 45.0, 35.0),
];

/// 两个阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    One,
    Two,
}

/// 分数格式第一阶段的停止序列。
const SEVERITY_STOP: &str = "</severity>";
/// `<block>` 格式第一阶段的停止序列。
const BLOCK_STOP: &str = "</block>";

impl Format {
    /// 按分类器用的模型查分数表；表外的模型用 `<block>` 格式。
    pub(crate) fn for_model(model: &str) -> Self {
        let base = model
            .strip_suffix(models::ONE_MILLION_SUFFIX)
            .unwrap_or(model);
        SEVERITY_TABLE
            .iter()
            .find(|(name, _, _)| *name == base)
            .map_or(Self::Block, |&(_, t1, t2)| Self::Severity { t1, t2 })
    }

    /// 组装好的提示：共用的正文（按用户规则渲染）接这个格式的输出说明。
    pub(crate) fn system(self, rules: UserRules<'_>) -> String {
        let output = match self {
            Self::Severity { .. } => auto_mode::output_severity(),
            Self::Block => auto_mode::output_block(),
        };
        format!("{}{output}", render(auto_mode::system(), rules))
    }

    /// 阶段的后缀（user 消息的最后一块）。
    pub(crate) fn suffix(self, stage: Stage) -> &'static str {
        match (self, stage) {
            (Self::Severity { .. }, Stage::One) => auto_mode::stage1_severity(),
            (Self::Severity { .. }, Stage::Two) => auto_mode::stage2_severity(),
            (Self::Block, Stage::One) => auto_mode::stage1_block(),
            (Self::Block, Stage::Two) => auto_mode::stage2_block(),
        }
    }

    /// 第一阶段的停止序列；第二阶段没有。
    pub(crate) fn stop(self, stage: Stage) -> Vec<String> {
        match (self, stage) {
            (Self::Severity { .. }, Stage::One) => vec![SEVERITY_STOP.to_owned()],
            (Self::Block, Stage::One) => vec![BLOCK_STOP.to_owned()],
            (_, Stage::Two) => Vec::new(),
        }
    }
}

/// 设置里的一组规则。
type Group = fn(&AutoModeRules) -> &[String];

/// 模板里用户规则的四个槽：标签名与设置里的哪一组。
const SLOTS: [(&str, Group); 4] = [
    ("user_environment_to_replace", |rules| &rules.environment),
    ("user_hard_deny_rules_to_replace", |rules| &rules.hard_deny),
    ("user_soft_deny_rules_to_replace", |rules| &rules.soft_deny),
    ("user_allow_rules_to_replace", |rules| &rules.allow),
];
/// deny 规则那一行在模板里的位置。
const DENY_RULES_SLOT: &str = "<settings_deny_rules>";
/// 设置里写它的位置放回槽的缺省。
const DEFAULTS: &str = "$defaults";
/// deny 规则那一行的模板变量。
const RULES_VAR: &str = "{{rules}}";

/// 按用户规则渲染模板：每个槽换成设置里的那一组（每项一行 `- <项>`，空时用缺省，`$defaults` 的位置放回缺省、只放第一次）；
/// deny 规则那一行在没有 deny 规则时为空。
fn render(template: &str, rules: UserRules<'_>) -> String {
    let mut out = template.to_owned();
    for (tag, group) in SLOTS {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        let (Some(start), Some(end)) = (out.find(&open), out.find(&close)) else {
            continue;
        };
        let value = slot(group(rules.auto_mode), &out[start + open.len()..end]);
        out.replace_range(start..end + close.len(), &value);
    }
    out.replacen(DENY_RULES_SLOT, &deny_rules(rules.deny), 1)
}

fn slot(items: &[String], default: &str) -> String {
    if items.is_empty() {
        return default.to_owned();
    }
    let mut lines = Vec::new();
    let mut defaults_placed = false;
    for item in items {
        if item == DEFAULTS {
            if !defaults_placed && !default.is_empty() {
                lines.push(default.to_owned());
            }
            defaults_placed = true;
        } else {
            lines.push(format!("- {item}"));
        }
    }
    lines.join("\n")
}

fn deny_rules(rules: &[String]) -> String {
    if rules.is_empty() {
        return String::new();
    }
    let listed: Vec<String> = rules.iter().map(|rule| format!("`{rule}`")).collect();
    auto_mode::deny_rules().replace(RULES_VAR, &listed.join(", "))
}

/// 提示里的规则行：行首 `- <名字> [...]:`（删除规则的第一行前面可能还有替换槽的标签）。
static RULE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(clippy::expect_used, reason = "固定的正则")]
    Regex::new(r"(?m)^(?:<user_(?:hard|soft)_deny_rules_to_replace>)?- ([^:\[\n]{1,60}?)\s*(?:\[[^\]\n]*\])?:\s")
        .expect("valid rule line pattern")
});

/// 认得的拦截规则与它们在提示里的写法（slug → 规则名）。
pub(crate) struct RuleNames {
    names: HashMap<String, String>,
}

impl RuleNames {
    /// 从组装好的提示里按 slug 取规则名；只收认得的 slug，同一个 slug 取第一次出现的写法。
    pub(crate) fn from_prompt(prompt: &str) -> Self {
        let mut names = HashMap::new();
        for found in RULE_LINE.captures_iter(prompt) {
            let Some(name) = found.get(1) else { continue };
            let key = slug(name.as_str());
            if known(&key) {
                names
                    .entry(key)
                    .or_insert_with(|| name.as_str().trim().to_owned());
            }
        }
        Self { names }
    }

    pub(crate) fn name(&self, slug: &str) -> Option<&str> {
        self.names.get(slug).map(String::as_str)
    }
}

/// 这个 slug 是不是认得的拦截规则（数据文件 `categories.json`）。
pub(crate) fn known(slug: &str) -> bool {
    auto_mode::categories().iter().any(|known| known == slug)
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
