//! 自动模式分类器的提示（`packages/rust-cli/prompts/auto_mode/`，docs/specs/rust-cli/auto-classifier.md §15.4）。
//!
//! `system`、`output_severity`、`stage1_severity`、`stage2_severity` 由 `scripts/rust-cli/prompts/auto_mode.mjs` 从基线生成
//! （`--check` 检查漂移）；`<block>` 格式的几段、压缩摘要的头一行、认得的规则 slug 与 deny 规则那一行手工维护。

use std::sync::OnceLock;

use rust_cli_base::include_lf;

use crate::trim;

/// 提示的模板，到输出说明的标题为止（含）：两种输出格式共用。用户规则的四个槽（`<user_…_to_replace>` 包着缺省）与 deny 规则的一行
/// （`<settings_deny_rules>`）留着，由分类器按设置渲染（docs/specs/rust-cli/auto-classifier.md §16.7）。
pub fn system() -> &'static str {
    trim(include_lf!("../../../prompts/auto_mode/system.txt"))
}

/// 分数格式的输出说明（接在 [`system`] 之后）。
pub fn output_severity() -> &'static str {
    trim(include_lf!(
        "../../../prompts/auto_mode/output_severity.txt"
    ))
}

/// `<block>` 格式的输出说明（接在 [`system`] 之后）。
pub fn output_block() -> &'static str {
    trim(include_lf!("../../../prompts/auto_mode/output_block.txt"))
}

/// 分数格式第一阶段的后缀。
pub fn stage1_severity() -> &'static str {
    trim(include_lf!(
        "../../../prompts/auto_mode/stage1_severity.txt"
    ))
}

/// 分数格式第二阶段的后缀。
pub fn stage2_severity() -> &'static str {
    trim(include_lf!(
        "../../../prompts/auto_mode/stage2_severity.txt"
    ))
}

/// `<block>` 格式第一阶段的后缀（两阶段都跑时）。
pub fn stage1_block() -> &'static str {
    trim(include_lf!("../../../prompts/auto_mode/stage1_block.txt"))
}

/// `<block>` 格式第二阶段的后缀。
pub fn stage2_block() -> &'static str {
    trim(include_lf!("../../../prompts/auto_mode/stage2_block.txt"))
}

/// 提示里列出用户 deny 规则的一行：`{{rules}}` 是反引号包着、`, ` 连接的规则串。
pub fn deny_rules() -> &'static str {
    trim(include_lf!("../../../prompts/auto_mode/deny_rules.txt"))
}

/// 摘录里压缩摘要的头一行。
pub fn compaction_summary() -> &'static str {
    trim(include_lf!(
        "../../../prompts/auto_mode/compaction_summary.txt"
    ))
}

/// 认得的拦截规则的 slug：分类器回复的类别只认这些。数据文件读不出时为空（什么类别都不认，理由按没有类别写）。
pub fn categories() -> &'static [String] {
    static CATEGORIES: OnceLock<Vec<String>> = OnceLock::new();
    CATEGORIES.get_or_init(|| {
        serde_json::from_str(include_lf!("../../../prompts/auto_mode/categories.json"))
            .unwrap_or_default()
    })
}

#[cfg(test)]
#[path = "auto_mode_tests.rs"]
mod tests;
