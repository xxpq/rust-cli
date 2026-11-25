//! 分类器给判决链的理由文字（docs/specs/rust-cli/auto-classifier.md §15.7）：判决链把它放进给模型的拒绝说明。

/// 分数格式拦下、回复里没有认得的类别。
pub(crate) const BLOCKED: &str = "Blocked by classifier";
/// 第一阶段判拦下时的理由（没有类别或 `<reason>`）。
pub(crate) const BLOCKED_FAST: &str = "Blocked by fast classifier";
/// `<block>` 格式拦下、回复里没有 `<reason>`。
pub(crate) const NO_REASON: &str = "No reason provided";
/// 第二阶段的请求失败、第一阶段已有回复：照第一阶段拦下。
pub(crate) const STAGE_TWO_ERROR: &str = "Stage 2 classifier error - blocking based on stage 1 assessment (usually transient \u{2014} retrying often succeeds)";

/// 同上，服务端给了等待时间。
pub(crate) fn stage_two_error_waiting(retry_after_ms: u64) -> String {
    format!(
        "Stage 2 classifier error - blocking based on stage 1 assessment (the API told the classifier to wait {} from now, so do not retry before then)",
        rust_cli_kernel_api::classifier::wait_text(retry_after_ms)
    )
}

#[cfg(test)]
#[path = "texts_tests.rs"]
mod tests;
