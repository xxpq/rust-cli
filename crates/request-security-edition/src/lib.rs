//! 发行版的模型请求处理（docs/specs/rust-cli/export.md §12）：这个构建没有，模型请求原样发送。

use std::sync::Arc;

use rust_cli_kernel_api::clock::Clock;
use rust_cli_model::RequestGuard;
use rust_cli_net::Egress;

/// 入口注入模型端口的处理（model 的 `RequestGuard`）：这个构建没有。
pub fn request_guard(
    _egress: Arc<Egress>,
    _clock: Arc<dyn Clock>,
) -> Option<Arc<dyn RequestGuard>> {
    None
}
