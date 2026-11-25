//! 发行版对模型请求的处理（docs/specs/rust-cli/model.md §5.4）：每次物理发送之前改写请求，非 2xx 之后决定立即再发。
//! 入口注入实现（[`crate::RouteOptions::guard`]）；没有注入时请求原样发。model 不解释它改写的头。

use async_trait::async_trait;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::ModelEndpoint;
use rust_cli_net::HttpRequest;

/// 入口注入的处理：每个逻辑请求按端点开一次；端点换了凭据（401 续期、拒绝访问之后问来源）时按新端点重开。
pub trait RequestGuard: Send + Sync {
    /// 这个端点要不要处理；不要时为 None（这个请求原样发）。
    fn open(&self, endpoint: &ModelEndpoint) -> Option<Box<dyn GuardedRequest>>;
}

/// 一个逻辑请求的处理：两次发送之间的状态归它（transport 的 `Connect` 持有）。
#[async_trait]
pub trait GuardedRequest: Send + Sync {
    /// 这次物理发送之前改写请求（加、换、去掉头）。普通重试与非流式重发的每次发送都调。失败时请求以这个错误结束，不发、不重试。
    async fn prepare(&mut self, request: &mut HttpRequest) -> Result<(), ModelError>;

    /// 非 2xx 的回复（正文已读，最多 `ERROR_BODY_LIMIT` 字节）：真时 transport 立即再发一次——不计重试次数、不等待、不发重试
    /// 通知，再发之前照样 [`GuardedRequest::prepare`]；假时照旧分类与处置。
    fn resend(&mut self, status: u16, body: &str) -> bool;
}
