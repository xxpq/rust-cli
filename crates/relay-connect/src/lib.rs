//! 执行中转（docs/specs/rust-cli/remote-exec.md §29.7）：经中转到达沙盒里的 `zcode executor`。与 ssh-connect 同一维：
//! 回答「怎么到达」，帧载体是 exec-transport 的 ws（§29.2）。
//!
//! 帧怎么搬归 exec-transport 的 ws，等多久与重连归 exec-client（§29.3、§29.6）。这里只做两件事：拨号时带上票与账号；
//! 拨号被拒时，按中转的状态码决定先拉起沙盒、放弃，还是交给 exec-client 重连。连上之后的关闭码不在这里分类：
//! exec-client 对断开只记原因、一律重连，分了类也没有用处（§29.7）。
//!
//! 没有部署这一步：沙盒由宿主拉起（z-go `ensureUp` 后传 `skip_up`）。第一次连接前不 `/up`：`/up` 有副作用（镜像不一致
//! 时会 RestartFunction、杀掉正在跑的执行器），重复调会让宿主的 `skip_up` 失效（§29.10 的 Q6，2026-11-05 先放进部署器、
//! 读了中转一方的仓库后撤回）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::proto::{http, mime};
use rust_cli_base::secret::RedactedString;
use rust_cli_exec_transport::{Connector, Link, LinkError, ws};
use rust_cli_net::websocket::WsError;
use rust_cli_net::{ERROR_BODY_LIMIT, Egress, HeaderValue, HttpRequest};

/// 执行连接的子协议。
pub const SUBPROTOCOL: &str = "zcode-exec.v1";
/// 代表哪个账号连进来。
const USER_HEADER: &str = "x-user-id";
/// `/up` 请求体里的 `flags`：中转契约原文给的值。
const UP_FLAGS: [&str; 1] = ["general_agent"];
/// 拨号时再连也一样的状态码：请求不对、票不对、没权限、对话不存在、沙盒还在旧镜像上。
const TERMINAL: [u16; 5] = [
    http::BAD_REQUEST,
    http::UNAUTHORIZED,
    http::FORBIDDEN,
    http::NOT_FOUND,
    http::PRECONDITION_FAILED,
];

/// 连到哪个沙盒。
#[derive(Clone)]
pub struct RelayTarget {
    /// 完整的 WebSocket 地址（对话在路径里，由下发配置的人填好）。
    pub url: String,
    /// 服务票。
    pub token: RedactedString,
    /// `X-User-Id` 的值。
    pub user: String,
    /// 拉起沙盒的地址（中转的 `/up`），只在拨号拿到 409（沙盒在会话中途被回收）时用；没有时 409 就交给 exec-client 重连。
    pub up_url: Option<String>,
}

/// 中转上的一个沙盒执行器。
pub struct Relay {
    egress: Arc<Egress>,
    target: RelayTarget,
}

impl Relay {
    pub fn new(egress: Arc<Egress>, target: RelayTarget) -> Self {
        Self { egress, target }
    }

    async fn dial(&self) -> Result<Box<dyn Link>, WsError> {
        let bearer = RedactedString::new(format!("Bearer {}", self.target.token.expose()));
        let headers = [
            (
                http::header::AUTHORIZATION.to_owned(),
                HeaderValue::Secret(bearer),
            ),
            (
                USER_HEADER.to_owned(),
                HeaderValue::from(self.target.user.as_str()),
            ),
        ];
        ws::dial(
            &self.egress,
            &self.target.url,
            &[SUBPROTOCOL.to_owned()],
            &headers,
        )
        .await
    }

    /// 拉起沙盒：`POST <up_url>`，头与请求体照中转契约原文。
    async fn bring_up(&self, up_url: &str) -> Result<(), LinkError> {
        let body = serde_json::json!({ "user_id": self.target.user, "flags": UP_FLAGS });
        let mut request = HttpRequest::post(up_url, body.to_string());
        request.headers = vec![
            (
                USER_HEADER.to_owned(),
                HeaderValue::from(self.target.user.as_str()),
            ),
            (
                http::header::CONTENT_TYPE.to_owned(),
                HeaderValue::from(mime::JSON),
            ),
        ];
        let response = self
            .egress
            .send(request)
            .await
            .map_err(|error| LinkError::Connect(format!("{up_url}: {error}")))?;
        let status = response.status();
        if http::SUCCESS.contains(&status) {
            tracing::info!(target: "relay_connect", up_url, status, "the sandbox was brought up");
            return Ok(());
        }
        let body = response.text(ERROR_BODY_LIMIT).await.unwrap_or_default();
        Err(LinkError::Connect(format!(
            "{up_url}: bringing the sandbox up answered {}",
            answer(status, &body)
        )))
    }

    /// 拨号失败 → 传输的错误：[`TERMINAL`] 里的是 `Rejected`（exec-client 不再重连），其余是 `Connect`（退避重连）。
    fn refused(&self, error: &WsError) -> LinkError {
        let url = &self.target.url;
        match error {
            WsError::Rejected { status, body } => {
                let text = format!("{url}: the relay answered {}", answer(*status, body));
                if TERMINAL.contains(status) {
                    LinkError::Rejected(text)
                } else {
                    LinkError::Connect(text)
                }
            }
            other => LinkError::Connect(format!("{url}: {other}")),
        }
    }
}

#[async_trait]
impl Connector for Relay {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        let first = self.dial().await;
        let stopped =
            matches!(&first, Err(WsError::Rejected { status, .. }) if *status == http::CONFLICT);
        let result = match (&self.target.up_url, stopped) {
            // 沙盒停了（409 needs_up）：拉起来再拨一次。第二次仍被拒就照常交出去，不来回。
            (Some(up_url), true) => {
                self.bring_up(up_url).await?;
                self.dial().await
            }
            _ => first,
        };
        result.map_err(|error| self.refused(&error))
    }

    fn describe(&self) -> String {
        self.target.url.clone()
    }
}

/// 状态码加中转写在响应体里的原因（例如 `chat_not_found`）。
fn answer(status: u16, body: &str) -> String {
    match body.trim() {
        "" => status.to_string(),
        reason => format!("{status}: {reason}"),
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
