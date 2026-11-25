//! 取内容（plugin-store.md §9.3、§9.4）：经入口的同一个出口下载（市场清单、zip、GitHub Archive），经 proc 跑系统 git。
//! 跳转手动跟（最多 5 次，跨 origin 去掉条目给的请求头），大小有上限，每块之间看取消。

use std::path::{Path, PathBuf};
use std::time::Duration;

use rust_cli_base::names::env;
use rust_cli_base::proto::http::{self as status, header};
use rust_cli_net::{Egress, HttpRequest, NetworkPolicy, NoProxy};
use tokio_util::sync::CancellationToken;

use super::zip::{extract, zip_root};
use crate::core::diagnostic::StoreError;
use crate::core::source::github_repository;

/// 插件 zip 的下载上限（Node `ZIP_DOWNLOAD_MAX_BYTES`）。
pub(crate) const ZIP_MAX_BYTES: usize = 200 * 1024 * 1024;
/// 市场清单的下载上限（Node `MARKETPLACE_JSON_MAX_BYTES`）。
pub(crate) const CATALOG_MAX_BYTES: usize = 10 * 1024 * 1024;
/// 跳转次数的上限（Node `ZIP_MAX_REDIRECTS`、`MARKETPLACE_JSON_MAX_REDIRECTS`）。
const MAX_REDIRECTS: usize = 5;
/// 条目不能给的请求头（Node `ZIP_DENIED_HEADERS`）。
const DENIED_HEADERS: [&str; 4] = [
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
];
/// GitHub Archive（Node `buildGitHubArchiveUrl`）。
const GITHUB_API_REPOS: &str = "https://api.github.com/repos";
const ARCHIVE_ACCEPT: &str = "application/vnd.github+json";
const INSTALLER_AGENT: &str = "ZCode-Plugin-Installer";
/// 没有 pin 时取的引用。
const DEFAULT_PIN: &str = "HEAD";
/// 这些状态码说明 Archive 拿不到（私有仓库、没有这个引用），改用系统 git（Node `shouldFallbackGitHubArchiveToGit`）。
const ARCHIVE_FALLBACK_STATUS: [u16; 3] =
    [status::UNAUTHORIZED, status::FORBIDDEN, status::NOT_FOUND];
/// 一条 git 命令的上限（Node `GIT_COMMAND_TIMEOUT_MS`）。
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);
/// 系统 git 的程序名。
const GIT: &str = "git";
/// `GIT_TERMINAL_PROMPT` 的「不问」：git 要凭据时直接失败，不在终端里等输入。
const GIT_NO_PROMPT: &str = "0";
/// 给 git 设代理的变量（Node `applyNetworkEgressEnv` 的 `ALL_PROXY_KEYS`）：curl 只认小写的 `http_proxy`，大写的给别的实现。
const PROXY_VARS: [&str; 6] = [
    env::HTTP_PROXY,
    env::HTTPS_PROXY,
    env::ALL_PROXY,
    env::HTTP_PROXY_LOWER,
    env::HTTPS_PROXY_LOWER,
    env::ALL_PROXY_LOWER,
];
/// 不走代理的规则（Node 的 `NO_PROXY_KEYS`）。
const NO_PROXY_VARS: [&str; 2] = [env::NO_PROXY, env::NO_PROXY_LOWER];

/// 下载失败：带状态码的（服务器回了非 2xx）与别的。
pub(crate) enum Failure {
    Status(u16, String),
    Other(StoreError),
}

impl From<Failure> for StoreError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Status(_, message) => StoreError::Fetch(message),
            Failure::Other(error) => error,
        }
    }
}

/// `http` 只给本机（测试与本机服务），其余只认 `https`（Node `validateZipDownloadUrl`）。
fn allowed_url(url: &url::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => url.host().is_some_and(|host| match host {
            url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        }),
        _ => false,
    }
}

/// GET `url`，跟跳转，正文最多 `limit` 字节。`what` 进错误文字（`plugin zip`、`marketplace manifest`）。
pub(crate) async fn download(
    egress: &Egress,
    url: &str,
    headers: &[(String, String)],
    (limit, what): (usize, &str),
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Failure> {
    let other = |message: String| Failure::Other(StoreError::Fetch(message));
    if let Some((name, _)) = headers
        .iter()
        .find(|(name, _)| DENIED_HEADERS.contains(&name.to_ascii_lowercase().as_str()))
    {
        return Err(Failure::Other(StoreError::Invalid(format!(
            "Plugin zip source header is not allowed: {name}"
        ))));
    }
    let mut current =
        url::Url::parse(url).map_err(|_| other(format!("The {what} URL is invalid: {url}")))?;
    let mut headers = headers.to_vec();
    for _ in 0..=MAX_REDIRECTS {
        if !allowed_url(&current) {
            return Err(other(format!("The {what} URL must be HTTPS: {current}")));
        }
        let mut request = HttpRequest::get(current.as_str());
        request.timeout = Some(rust_cli_net::timeout::PLUGIN_DOWNLOAD);
        for (name, value) in &headers {
            request = request.header(name.clone(), value.clone());
        }
        let mut response = tokio::select! {
            response = egress.send(request) => response.map_err(|error| other(format!("Failed to download {what}: {error}")))?,
            () = cancel.cancelled() => return Err(Failure::Other(StoreError::Cancelled)),
        };
        let code = response.status();
        if status::is_redirect(code) {
            let Some(location) = response.headers().get("location") else {
                return Err(other(format!(
                    "The {what} redirect is missing a Location header: {current}"
                )));
            };
            let next = current
                .join(location)
                .map_err(|_| other(format!("The {what} redirect is invalid: {location}")))?;
            if next.origin() != current.origin() {
                headers.clear();
            }
            current = next;
            continue;
        }
        if !response.is_success() {
            return Err(Failure::Status(
                code,
                format!("Failed to download {what}: {code} ({current})"),
            ));
        }
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk.map_err(|error| other(format!("Failed to download {what}: {error}")))?,
                () = cancel.cancelled() => return Err(Failure::Other(StoreError::Cancelled)),
            };
            let Some(chunk) = chunk else {
                break;
            };
            if body.len() + chunk.len() > limit {
                return Err(other(format!(
                    "The {what} exceeds the size limit of {limit} bytes: {url}"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(body);
    }
    Err(other(format!(
        "The {what} download exceeded the redirect limit: {url}"
    )))
}

/// 一个仓库的内容放进 `work`（新建），返回仓库根：公开 GitHub 仓库先试 Archive，拿不到或要 git 的语义时用系统 git。
pub(crate) async fn repository(
    egress: &Egress,
    repo: &Repo<'_>,
    work: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf, StoreError> {
    if repo.sparse.is_empty()
        && let Some((owner, name)) = github_repository(repo.url)
    {
        let pin = repo.sha.or(repo.git_ref).unwrap_or(DEFAULT_PIN);
        match archive(
            egress,
            (&owner, &name, pin),
            &work.join("archive"),
            repo.path,
            cancel,
        )
        .await
        {
            Ok(root) => return Ok(root),
            Err(ArchiveFailure::NeedsGit(reason)) => {
                tracing::info!(
                    event = "plugins.archive.fallback",
                    url = repo.url,
                    reason,
                    "GitHub Archive falls back to git"
                );
            }
            Err(ArchiveFailure::Failed(StoreError::Cancelled)) => {
                return Err(StoreError::Cancelled);
            }
            Err(ArchiveFailure::Failed(error)) => {
                return Err(StoreError::Fetch(format!(
                    "Failed to fetch GitHub archive for {}: {error}",
                    repo.url
                )));
            }
        }
    }
    let root = clone(
        repo,
        &work.join("git"),
        &git_proxy_env(&egress.policy()),
        cancel,
    )
    .await?;
    Ok(root)
}

/// 仓库的来源。
pub(crate) struct Repo<'a> {
    pub(crate) url: &'a str,
    pub(crate) git_ref: Option<&'a str>,
    pub(crate) sha: Option<&'a str>,
    /// 仓库里要的子目录（Archive 时用来查 LFS 的继承；git 时由调用方取）。
    pub(crate) path: Option<&'a str>,
    pub(crate) sparse: &'a [String],
}

enum ArchiveFailure {
    NeedsGit(String),
    Failed(StoreError),
}

/// GitHub Archive（zipball）：只有一个顶层目录，去掉它；仓库要子模块或 LFS 时改用 git（Node `detectRequiredGitSemantics`）。
async fn archive(
    egress: &Egress,
    (owner, name, pin): (&str, &str, &str),
    target: &Path,
    path: Option<&str>,
    cancel: &CancellationToken,
) -> Result<PathBuf, ArchiveFailure> {
    let url = format!(
        "{GITHUB_API_REPOS}/{owner}/{name}/zipball/{}",
        rust_cli_base::js::uri::encode_uri_component(pin)
    );
    let headers = [
        (header::ACCEPT.to_owned(), ARCHIVE_ACCEPT.to_owned()),
        (header::USER_AGENT.to_owned(), INSTALLER_AGENT.to_owned()),
    ];
    let bytes = match download(
        egress,
        &url,
        &headers,
        (ZIP_MAX_BYTES, "plugin zip"),
        cancel,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(Failure::Status(code, message)) if ARCHIVE_FALLBACK_STATUS.contains(&code) => {
            return Err(ArchiveFailure::NeedsGit(message));
        }
        Err(failure) => return Err(ArchiveFailure::Failed(failure.into())),
    };
    let extracted = match extract(bytes, target, cancel).await {
        Ok(extracted) => extracted,
        Err(StoreError::Invalid(message))
            if message.contains("symlinks are not supported")
                || message.contains("Unsupported plugin zip entry type") =>
        {
            return Err(ArchiveFailure::NeedsGit(message));
        }
        Err(error) => return Err(ArchiveFailure::Failed(error)),
    };
    let root = zip_root(target, &extracted, None, Some(true), true)
        .await
        .map_err(ArchiveFailure::Failed)?;
    if let Some(reason) = git_semantics(&root, path).await {
        return Err(ArchiveFailure::NeedsGit(reason.to_owned()));
    }
    Ok(root)
}

/// 仓库要 git 才能还原的东西：子模块、LFS（根与选中的子目录一路上的 `.gitattributes`）。
async fn git_semantics(root: &Path, path: Option<&str>) -> Option<&'static str> {
    if rust_cli_fs::is_file(&root.join(".gitmodules")).await {
        return Some("repository declares Git submodules");
    }
    let mut dir = root.to_path_buf();
    let mut dirs = vec![dir.clone()];
    for part in path
        .unwrap_or_default()
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
    {
        dir = dir.join(part);
        dirs.push(dir.clone());
    }
    for dir in dirs {
        let attributes = rust_cli_fs::read_optional(&dir.join(".gitattributes"))
            .await
            .ok()
            .flatten();
        if attributes.is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains("filter=lfs")) {
            return Some("repository declares Git LFS filters");
        }
    }
    None
}

/// 系统 git 的代理（plugin-store.md §9.4）：与出口同一份策略（入口按 ZCode 配置里的代理算出），git 与下载走同一个代理。
/// 修 bug（主会话定，plugin-store.md §15 第 6 项）：以前 git 只继承进程环境，App 里只配了 ZCode 的代理（`ZCODE_HTTP_PROXY`）时
/// clone 直连，在只能经代理出网的机器上安装与刷新市场失败。没有代理时什么都不设：入口算代理时已经读过标准变量。
fn git_proxy_env(policy: &NetworkPolicy) -> Vec<(&'static str, String)> {
    let Some(proxy) = &policy.proxy else {
        return Vec::new();
    };
    let mut vars: Vec<(&'static str, String)> = PROXY_VARS
        .iter()
        .map(|name| (*name, proxy.clone()))
        .collect();
    if let NoProxy::Node(rules) | NoProxy::Standard(rules) = &policy.no_proxy {
        vars.extend(NO_PROXY_VARS.iter().map(|name| (*name, rules.clone())));
    }
    vars
}

/// 系统 git clone 到 `target`：没有 sha 时 `--depth 1`；`--branch <ref>`；有 `sparse` 时 `--filter=blob:none --sparse` 再
/// `sparse-checkout set`；有 sha 时 clone 后 `checkout <sha>`。`proxy` 是给每条 git 命令的代理变量。
async fn clone(
    repo: &Repo<'_>,
    target: &Path,
    proxy: &[(&'static str, String)],
    cancel: &CancellationToken,
) -> Result<PathBuf, StoreError> {
    let target_text = target.to_string_lossy().into_owned();
    let mut args: Vec<String> = vec!["clone".into()];
    if repo.sha.is_none() {
        args.extend(["--depth".into(), "1".into()]);
    }
    if let Some(git_ref) = repo.git_ref {
        args.extend(["--branch".into(), git_ref.into()]);
    }
    if !repo.sparse.is_empty() {
        args.extend(["--filter=blob:none".into(), "--sparse".into()]);
    }
    args.extend([repo.url.into(), target_text.clone()]);
    git(&args, proxy, cancel).await?;
    if !repo.sparse.is_empty() {
        let mut sparse: Vec<String> = vec![
            "-C".into(),
            target_text.clone(),
            "sparse-checkout".into(),
            "set".into(),
        ];
        sparse.extend(repo.sparse.iter().cloned());
        git(&sparse, proxy, cancel).await?;
    }
    if let Some(sha) = repo.sha {
        git(
            &["-C".into(), target_text, "checkout".into(), sha.into()],
            proxy,
            cancel,
        )
        .await?;
    }
    Ok(target.to_path_buf())
}

/// 跑一条 git 命令：取消时杀掉；超过上限算失败。不让 git 等终端里的输入（凭据提示）。
async fn git(
    args: &[String],
    proxy: &[(&'static str, String)],
    cancel: &CancellationToken,
) -> Result<(), StoreError> {
    let mut command = rust_cli_proc::command(GIT);
    command
        .args(args)
        .envs(proxy.iter().map(|(name, value)| (*name, value.as_str())))
        .env(
            rust_cli_base::names::env::GIT_TERMINAL_PROMPT,
            GIT_NO_PROMPT,
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::GitUnavailable(error.to_string()));
        }
        Err(error) => {
            return Err(StoreError::Fetch(format!(
                "git {}: {error}",
                args.join(" ")
            )));
        }
    };
    let output = tokio::select! {
        output = tokio::time::timeout(GIT_COMMAND_TIMEOUT, child.wait_with_output()) => output,
        () = cancel.cancelled() => return Err(StoreError::Cancelled),
    };
    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(StoreError::Fetch(format!(
                "git {}: {error}",
                args.join(" ")
            )));
        }
        Err(_) => {
            return Err(StoreError::Fetch(format!(
                "git {} timed out after {} s",
                args.join(" "),
                GIT_COMMAND_TIMEOUT.as_secs()
            )));
        }
    };
    if output.status.success() {
        return Ok(());
    }
    Err(StoreError::Fetch(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
