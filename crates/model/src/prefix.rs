//! 主线程请求的已定前缀在模型端口一侧（docs/specs/rust-cli/request-prefix.md §6）：取端点之后按这次端点的编码身份决定
//! 拼前缀还是整段，编码之后把下一步的前缀交回计划。

use std::sync::Arc;

use rust_cli_kernel_api::model::{ModelError, ModelRequest};
use rust_cli_kernel_api::provider::{ModelEndpoint, WireProtocol};
use rust_cli_kernel_api::request_prefix::{PlanRef, PrefixPlan};
use rust_cli_net::HttpRequest;

use crate::anthropic::{self, Anthropic, request::Splice};
use crate::profiles::Profile;
use crate::protocol::Protocol;

/// 这次请求怎样用前缀。
pub(crate) struct Fitted {
    plan: Arc<dyn PrefixPlan>,
    /// 为真时消息已经换成整段（[`PrefixPlan::full`]），不拼旧前缀，按这次的身份重写一份。
    rebuilt: bool,
    settle: usize,
    identity: u32,
}

/// 取端点之后、换图片占位之前（request-prefix.md §6）：
/// - Anthropic 的端点、身份与计划的前缀相同、不降级 system 消息：拼前缀；
/// - Anthropic 的端点、不降级 system 消息，但身份不同（换了方案或端点能力）：消息换成整段，按这次的身份重写前缀；
/// - 别的协议、或要降级 system 消息：消息换成整段，不交回前缀（下一步整段）。
pub(crate) async fn fit(
    plan: PlanRef,
    request: &mut ModelRequest,
    endpoint: &ModelEndpoint,
    profile: &Profile,
) -> Option<Fitted> {
    let plan = plan.0;
    let prefix = plan
        .wire()
        .map(|wire| (wire.identity, wire.messages, wire.flags));
    let anthropic = endpoint.protocol == WireProtocol::AnthropicMessages;
    let usable = anthropic
        && !anthropic::folds_system(endpoint, request, prefix.map_or(0, |(_, _, flags)| flags));
    let identity = anthropic::wire_identity(endpoint, profile);
    if usable && prefix.is_none_or(|(wire, _, _)| wire == identity) {
        let settle = plan.settle();
        return Some(Fitted {
            plan,
            rebuilt: false,
            settle,
            identity,
        });
    }
    if prefix.is_some() {
        request.messages = plan.full().await;
    }
    usable.then(|| Fitted {
        settle: prefix.map_or(0, |(_, messages, _)| messages) + plan.settle(),
        plan,
        rebuilt: true,
        identity,
    })
}

/// 编码（[`fit`] 之后、换过图片占位）：拼前缀，交回下一步的前缀。调试构建里拼了旧前缀的请求再整段编码一次，逐字节比
/// （request-prefix.md §6）。
pub(crate) async fn encode(
    fitted: Fitted,
    endpoint: &ModelEndpoint,
    request: ModelRequest,
    profile: &Profile,
) -> Result<HttpRequest, ModelError> {
    #[cfg(debug_assertions)]
    let shadow = (!fitted.rebuilt && fitted.plan.wire().is_some()).then(|| request.clone());
    let splice = Splice {
        wire: if fitted.rebuilt {
            None
        } else {
            fitted.plan.wire()
        },
        settle: fitted.settle,
        identity: fitted.identity,
    };
    // 每个请求一条，只在开发构建写出（logging.md §3）：拼了几条前缀、这次编码了几条。
    tracing::debug!(
        event = "request.prefix",
        spliced = splice.wire.map_or(0, |wire| wire.messages),
        encoded = request.messages.len(),
        rebuilt = fitted.rebuilt,
        "request messages spliced onto the settled prefix"
    );
    let (http, wire) = Anthropic::encode_with(endpoint, request, profile, Some(splice))?;
    #[cfg(debug_assertions)]
    if let Some(mut whole) = shadow {
        whole.messages = fitted.plan.full().await;
        crate::media::project(&mut whole.messages, endpoint);
        let expected = Anthropic.encode(endpoint, whole, profile)?;
        if expected.body != http.body || expected.headers != http.headers {
            let at = expected
                .body
                .iter()
                .zip(http.body.iter())
                .position(|(left, right)| left != right);
            tracing::error!(event = "request.prefix.mismatch", at = ?at, whole = expected.body.len(), spliced = http.body.len(), "the spliced request differs from the whole request");
            panic!("request.prefix.mismatch at {at:?}");
        }
    }
    if let Some(wire) = wire {
        fitted.plan.keep(wire);
    }
    Ok(http)
}

#[cfg(test)]
#[path = "prefix_tests.rs"]
mod tests;
