//! 内置账号 provider 的 ID（与 App 的 `BUILTIN_MODEL_PROVIDER_IDS`、Node 的 provider 配置同值）：平台层（套餐与用量）与模型
//! 适配器（Start Plan 的准入重试，docs/specs/rust-cli/provider-errors.md §5.3.2）都引用这里，不各写一份。

/// bigmodel 的 Start Plan（免费套餐）账号。
pub const BIGMODEL_START_PLAN: &str = "account:bigmodel-start-plan";
/// z.ai 的 Start Plan 账号。
pub const ZAI_START_PLAN: &str = "account:zai-start-plan";
/// Start Plan 的两个账号 provider。
pub const START_PLAN: [&str; 2] = [BIGMODEL_START_PLAN, ZAI_START_PLAN];
