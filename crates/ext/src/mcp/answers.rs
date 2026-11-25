//! 回答服务器发来的请求（docs/specs/rust-cli/ext.md §3.3、§3.11 的 G12）：rmcp 的服务循环把请求分派到这里。
//!
//! `ping` → `{}`；`roots/list` → 工作区根；其余一律 `-32601 Method not found: <方法>`。只按方法名回答：适配器把 `ping`
//! 以外的请求都交成 `CustomRequest`（`link.rs`），不看 rmcp 会把它解析成哪个类型。rmcp 缺省的回调对 elicitation 回
//! `decline`、对别的请求只写方法名，与以前的回答不同。前端声明了的 elicitation 与 sampling 由适配器截下（`asks.rs`，ext.md
//! §3.13.2），到这里的是没声明的，照旧 Method not found。

use std::collections::HashSet;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use rmcp::model::{
    ClientCapabilities, ClientResult, CustomResult, ElicitationCapability, ErrorCode, ErrorData,
    Implementation, InitializeRequestParams, ProtocolVersion, RootsCapabilities,
    SamplingCapability, ServerNotification, ServerRequest,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient, Service};
use rust_cli_kernel_api::env_files::PathStyle;
use serde_json::{Value, json};

/// `pathToFileURL` 要百分号编码的 ASCII（Bun 1.3 与 Node 24 实测，ext.md §3.2）：C0 控制字符与 DEL（`CONTROLS`）、空格、
/// `"`、`#`、`%`、`<`、`>`、`?`、`[`、`\`、`]`、`^`、`` ` ``、`{`、`|`、`}`、`~`。非 ASCII 的字节库总是编码。
const FILE_URL_PATH: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'~');

/// 这条连接的回答方与握手时声明的客户端信息。
pub(super) struct Answers {
    roots: Vec<String>,
    info: InitializeRequestParams,
}

impl Answers {
    /// `roots` 是 `roots/list` 的回答（工作区根）；`offered` 是 `initialize` 发的修订，`version` 是本工程的版本；`asked` 是前端声明的
    /// 服务器询问（ext.md §3.13.1，这两种请求由适配器截下，不到这里）。
    pub(super) fn new(
        roots: Vec<String>,
        offered: &ProtocolVersion,
        version: &str,
        (elicitation, sampling): (Option<ElicitationCapability>, Option<SamplingCapability>),
    ) -> Self {
        // 声明的能力是 `{roots: {}}` 加前端声明的询问（ext.md §3.3）：rmcp 的缺省值是空的。
        let mut capabilities = ClientCapabilities::default();
        capabilities.roots = Some(RootsCapabilities::default());
        capabilities.elicitation = elicitation;
        capabilities.sampling = sampling;
        let info = InitializeRequestParams::new(
            capabilities,
            Implementation::new(rust_cli_base::names::CLIENT_NAME, version),
        )
        .with_protocol_version(offered.clone());
        Self { roots, info }
    }
}

impl Service<RoleClient> for Answers {
    fn handle_request(
        &self,
        request: ServerRequest,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<ClientResult, ErrorData>> + Send + '_ {
        let answer = match request {
            ServerRequest::CustomRequest(request) => answer(&request.method, &self.roots)
                .map(|result| ClientResult::CustomResult(CustomResult(result))),
            // 适配器只把 `ping` 交成类型化的请求（握手时 rmcp 自己回答它，之后到这里）。
            _ => Ok(ClientResult::empty(())),
        };
        std::future::ready(answer)
    }

    /// 适配器在读的一侧按顺序处理通知，不交给 rmcp（§3.11 的 G11），这里收不到。
    fn handle_notification(
        &self,
        _notification: ServerNotification,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Ok(()))
    }

    fn get_info(&self) -> InitializeRequestParams {
        self.info.clone()
    }
}

/// 服务器发来的请求的回答：`ping`、`roots/list`；其余回 Method not found。
pub(super) fn answer(method: &str, roots: &[String]) -> Result<Value, ErrorData> {
    match method {
        "ping" => Ok(json!({})),
        "roots/list" => {
            // 按 href 去重，保留先后（F57）。
            let mut seen = HashSet::new();
            let roots: Vec<Value> = roots
                .iter()
                .map(|root| file_uri(root))
                .filter(|uri| seen.insert(uri.clone()))
                .map(|uri| json!({"uri": uri}))
                .collect();
            Ok(json!({ "roots": roots }))
        }
        other => Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            format!("Method not found: {other}"),
            None,
        )),
    }
}

/// 工作区根的 `file://` URI，同 `url.pathToFileURL(root).href`（docs/specs/rust-cli/ext.md §3.2）。风格按根的
/// 写法推断；Windows 的 `\\?\` 前缀去掉，UNC 的服务器名是主机（转小写）。
///
/// 缺陷修复（工作线 F57）：以前不做百分号编码（`/a b` 写成 `file:///a b`，含 `#`、`?`、`%` 的路径服务器解析不回来），
/// UNC 写成 `file:////server/share`。
pub(super) fn file_uri(path: &str) -> String {
    if PathStyle::infer(path) == PathStyle::Posix {
        return file_url(&encode_file_path(path));
    }
    let unc = path.strip_prefix(r"\\?\UNC\").or_else(|| {
        path.strip_prefix(r"\\")
            .filter(|rest| !rest.starts_with(r"?\"))
    });
    if let Some(unc) = unc {
        let (host, rest) = unc.split_at(unc.find(['\\', '/']).unwrap_or(unc.len()));
        let rest = encode_file_path(&rest.replace('\\', "/"));
        return format!("file://{}{rest}", host.to_ascii_lowercase());
    }
    let local = path.strip_prefix(r"\\?\").unwrap_or(path);
    file_url(&encode_file_path(&local.replace('\\', "/")))
}

/// 编码后的路径接在 `file://` 后面：不以 `/` 开头时（Windows 的 `C:/…`）补一个。
fn file_url(encoded: &str) -> String {
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

/// `pathToFileURL` 的百分号编码：[`FILE_URL_PATH`] 与非 ASCII 的 UTF-8 字节写成大写的 `%XX`。
fn encode_file_path(path: &str) -> String {
    utf8_percent_encode(path, FILE_URL_PATH).to_string()
}

#[cfg(test)]
#[path = "answers_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "answers_parity_tests.rs"]
mod parity_tests;
