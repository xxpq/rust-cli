//! 这一轮执行覆盖的单次尝试（docs/specs/rust-cli/model.md §5.2）：带退回规则的
//! 请求在交出内容之前失败时，按规则判要不要退回。端口认得出失败在哪一步与端点给的业务码，主循环只换选择（先例同 `fallback_model`）。

use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::turn_execution::ExecutionFallback;

/// 失败在哪一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stage {
    /// 取端点（还没发请求）：目录里没有这个选择、provider 不可执行，或者凭据的来源没给凭据。
    Endpoint,
    /// 请求本身（响应头之前，或者流里一个内容事件都没交出）：端点给的业务码，原样。
    Request(Option<String>),
}

/// 命中规则时换成 [`ModelError::ExecutionFellBack`]，否则原样。取消与超窗不退回：取消按停止收尾，超窗交给反应式压缩（同一个模型退回也一样超窗）。
/// 取端点时选择不可用（`InvalidRequest`，见 `ModelEndpoints::endpoint`）的原因是规则的 `unavailable`；取端点时别的失败（拿不到凭据）照请求失败判、没有业务码。
pub(crate) fn fell_back(
    rules: Option<&ExecutionFallback>,
    stage: Stage,
    error: ModelError,
) -> ModelError {
    let Some(rules) = rules else {
        return error;
    };
    let reason = match (&error, &stage) {
        (ModelError::Cancelled | ModelError::PromptTooLong { .. }, _) => None,
        (ModelError::InvalidRequest { .. }, Stage::Endpoint) => Some(rules.unavailable.as_str()),
        (_, Stage::Endpoint) => rules.reason(None),
        (_, Stage::Request(code)) => rules.reason(code.as_deref()),
    };
    match reason {
        Some(reason) => ModelError::ExecutionFellBack {
            reason: reason.to_owned(),
            error: Box::new(error),
        },
        None => error,
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
