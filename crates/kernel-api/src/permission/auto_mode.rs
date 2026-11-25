//! 自动模式的设置、生效与拒绝计数（docs/specs/rust-cli/auto-classifier.md §16.5、§16.6）。
//!
//! 设置由入口从配置文件读一次，放进判决链的规则快照（[`super::PermissionRules::auto_mode`]）；自动语义生不生效不存，每次判决按
//! 会话的执行状态、设置与主模型现算（[`auto_active`]）。拒绝计数的 owner 是会话 actor 与子代理的输出端，规则只有
//! [`DenialCounts::record`] 一份。

use serde::{Deserialize, Serialize};

use super::{ExecutionState, PermissionMode};
use rust_cli_base::models;

/// 自动模式的设置（`permission.*`，用户级配置与 `--settings`；项目文件不带）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoModeSettings {
    /// `permission.disableAutoMode: "disable"`：自动模式一律不生效。
    #[serde(default)]
    pub disabled: bool,
    /// `permission.useAutoModeDuringPlan`：plan 模式按自动语义走（缺省开）。
    #[serde(default = "enabled")]
    pub use_auto_mode_during_plan: bool,
    /// `permission.autoMode.classifyAllShell`：自动语义生效时全部 Bash 的 allow 规则都不生效。
    #[serde(default)]
    pub classify_all_shell: bool,
    /// `permission.autoMode` 的四组规则（进分类器的提示）。
    #[serde(default)]
    pub rules: AutoModeRules,
}

fn enabled() -> bool {
    true
}

impl Default for AutoModeSettings {
    fn default() -> Self {
        Self {
            disabled: false,
            use_auto_mode_during_plan: true,
            classify_all_shell: false,
            rules: AutoModeRules::default(),
        }
    }
}

/// 用户写的分类器规则：每组换掉提示里对应槽的缺省（空时用缺省，`$defaults` 的位置放回缺省）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoModeRules {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub soft_deny: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hard_deny: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<String>,
}

/// 这一刻自动语义生不生效：设置没有关、主模型支持；plan 开时看设置、且不是从 bypass 类模式进入的，plan 关时看模式是不是 auto。
/// `model` 是会话现在的主模型；没有时（输出端不持有会话）算支持。
pub fn auto_active(
    state: ExecutionState,
    settings: &AutoModeSettings,
    model: Option<&str>,
) -> bool {
    if settings.disabled || !model.is_none_or(supports_auto_mode) {
        return false;
    }
    if state.plan {
        let bypass = matches!(
            state.mode,
            PermissionMode::BypassPermissions | PermissionMode::Guarded
        );
        return settings.use_auto_mode_during_plan && !bypass;
    }
    state.mode == PermissionMode::Auto
}

/// 主模型支不支持自动模式：旧的 Anthropic 模型不支持；本工程的端点都按第三方算，另外排除两个模型与 haiku（Haiku 5.5 除外）。
/// 其余模型（GLM、DeepSeek 等）都支持。
pub fn supports_auto_mode(model: &str) -> bool {
    let id = canonical(model);
    if id.contains(models::CLAUDE_3_MARK) || models::AUTO_MODE_TOO_OLD.contains(&id.as_str()) {
        return false;
    }
    let third_party = models::AUTO_MODE_THIRD_PARTY_UNSUPPORTED.contains(&id.as_str())
        || (id.contains(models::HAIKU_MARK) && id != models::HAIKU_5_5);
    !third_party
}

/// 日期后缀（`-20250929`）的位数。
const DATE_SUFFIX_DIGITS: usize = 8;

/// 比较用的模型 ID：小写，去掉 `[1m]` 这类档位后缀与日期后缀。
fn canonical(model: &str) -> String {
    let id = model.split_once('[').map_or(model, |(id, _)| id);
    let id = id.to_ascii_lowercase();
    match id.rsplit_once('-') {
        Some((head, tail))
            if tail.len() == DATE_SUFFIX_DIGITS && tail.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head.to_owned()
        }
        _ => id,
    }
}

/// 连续拦下几次之后改问人。
pub const MAX_CONSECUTIVE_DENIALS: u32 = 3;
/// 一个会话（或一次子代理运行）累计拦下几次之后改问人。
pub const MAX_TOTAL_DENIALS: u32 = 20;

/// 自动模式的一次结论（计数只认这两种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoVerdict {
    /// 放行（快速放行或分类器放行）：清零连续数。
    Allowed,
    /// 分类器拦下（含解析不了）：连续、累计各加一。
    Blocked,
}

/// 到了哪个上限（带改完之后的次数，给日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialLimit {
    Consecutive(u32),
    Total(u32),
}

/// 拦下的次数：连续与累计。只在内存，会话重开从 0 起。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DenialCounts {
    pub consecutive: u32,
    pub total: u32,
}

impl DenialCounts {
    /// 记一次结论：放行清零连续数；拦下各加一，累计到上限时两个数清零、回 `Total`，否则连续到上限时回 `Consecutive`（不清零：
    /// 之后再拦下仍问人，直到一次放行）。
    #[must_use]
    pub fn record(self, verdict: AutoVerdict) -> (Self, Option<DenialLimit>) {
        match verdict {
            AutoVerdict::Allowed => (
                Self {
                    consecutive: 0,
                    ..self
                },
                None,
            ),
            AutoVerdict::Blocked => {
                let next = Self {
                    consecutive: self.consecutive.saturating_add(1),
                    total: self.total.saturating_add(1),
                };
                if next.total >= MAX_TOTAL_DENIALS {
                    (Self::default(), Some(DenialLimit::Total(next.total)))
                } else if next.consecutive >= MAX_CONSECUTIVE_DENIALS {
                    (next, Some(DenialLimit::Consecutive(next.consecutive)))
                } else {
                    (next, None)
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "auto_mode_tests.rs"]
mod tests;
