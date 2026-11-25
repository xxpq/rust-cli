//! 部署配置（docs/specs/rust-cli/deployment.md §6）：会话的主 env 与会话库在哪。缺省全本地，与没有这份配置时完全相同。
//!
//! 来源是环境变量 [`VAR`] 给的 JSON 文件；没设就是缺省。未知字段报错（`deny_unknown_fields`），拼错的键不会被悄悄
//! 忽略。密钥不写进这份文件：要用时写成 `*_env`（环境变量的名字）。角色不在这里，由子命令决定（deployment.md §14）。
//! 会话库的选择只有 `store` 键：写了 PG 就连 PG，连不上报错，不回退本机 SQLite。

use std::fmt;
use std::path::Path;
#[cfg(feature = "cloud")]
use std::sync::Arc;

use rust_cli_base::path::DRIVE_PREFIX_LEN as DRIVE_LEN;
use rust_cli_base::secret::{EnvSnapshot, RedactedString};
#[cfg(feature = "cloud")]
use rust_cli_pg_store::PgDatabase;
use serde::Deserialize;

use crate::commands::flags::Fatal;

/// 部署配置文件的路径。
pub(crate) const VAR: &str = rust_cli_base::names::env::ZCODE_DEPLOYMENT;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Deployment {
    #[serde(default)]
    pub primary_env: PrimaryEnv,
    #[serde(default)]
    pub store: SessionStoreConfig,
}

/// 会话的主 env：工具不写 `machine` 时在它上面执行，规则、记忆、skills、git 都从它读（deployment.md §8）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PrimaryEnv {
    /// 本进程里的 env-local（缺省）。
    #[default]
    Local,
    /// 经 SSH 连到的执行器（远端执行 R2 的部署与连接）。
    Ssh(SshPrimary),
    /// 执行中转后面那个沙盒里的执行器（F142，remote-exec.md §29.8）。按到达方式命名，与 `ssh` 同一维（§29.2）；旧名
    /// `remote` 仍可用，北京 staging 的云端运行壳的配置按它写。
    #[serde(alias = "remote")]
    Relay(RelayPrimary),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SshPrimary {
    /// ssh 的目标（`user@host`，或 ssh 配置里的 Host）。
    pub destination: String,
    /// 那台机器上的工作目录：会话的工作区根。
    pub workdir: String,
    /// 给 ssh 的 `-F <文件>`。
    #[serde(default)]
    pub ssh_config: Option<String>,
    /// 上传用的执行器产物；没有时按本机版本从 npm 取（F100）。
    #[serde(default)]
    pub artifact: Option<String>,
}

/// 经执行中转到达的沙盒：执行器在沙盒镜像里，由中转的网关拉起（remote-exec.md §29.8）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayPrimary {
    /// 中转的 WebSocket 地址，对话在路径里；CLI 不拼中转的路径。
    pub url: String,
    /// 放服务票的环境变量的名字：票不写进这份文件。
    pub token_env: String,
    /// `X-User-Id` 的值。
    pub user: String,
    /// 沙盒里的工作目录：会话的工作区根。
    pub workdir: String,
    /// 拉起沙盒的地址（中转的 `/up`）。
    #[serde(default)]
    pub up_url: Option<String>,
}

/// 会话库：本机 SQLite（缺省）或共用 PG（pg-store.md §11）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SessionStoreConfig {
    #[default]
    Local,
    Pg(PgStore),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PgStore {
    /// 放连接串的环境变量的名字：连接串带密码，不写进这份文件。
    pub url_env: String,
    /// 专用测试用户的稳定 ID；正式宿主从已验证的登录身份取得。
    pub user_id: String,
}

/// 读不到、写错或不合规则的部署配置：入口照原文报给用户，退出码 1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeploymentError {
    /// 配置文件的路径；缺省配置不会出错，所以总有。
    pub path: String,
    pub reason: String,
}

impl fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{VAR}={}: {}", self.path, self.reason)
    }
}

impl std::error::Error for DeploymentError {}

impl Deployment {
    /// 解析并校验一份配置的原文。
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let deployment: Self = serde_json::from_str(text).map_err(|error| error.to_string())?;
        deployment.validate()?;
        Ok(deployment)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        match &self.primary_env {
            PrimaryEnv::Local => {}
            PrimaryEnv::Ssh(ssh) => {
                if ssh.destination.trim().is_empty() {
                    return Err("primary_env.ssh.destination is empty".to_owned());
                }
                if !is_absolute(&ssh.workdir) {
                    return Err(format!(
                        "primary_env.ssh.workdir must be an absolute path on that machine, got {:?}",
                        ssh.workdir
                    ));
                }
            }
            PrimaryEnv::Relay(relay) => {
                check_relay_url(&relay.url)?;
                if relay.token_env.trim().is_empty() {
                    return Err("primary_env.relay.token_env is empty".to_owned());
                }
                check_relay_target(&relay.user, &relay.workdir)?;
            }
        }
        if let SessionStoreConfig::Pg(pg) = &self.store {
            if pg.url_env.trim().is_empty() {
                return Err("store.pg.url_env is empty".to_owned());
            }
            if pg.user_id.trim().is_empty() {
                return Err("store.pg.user_id is empty".to_owned());
            }
        }
        Ok(())
    }

    /// 主 env 就在本进程里。
    pub(crate) fn primary_is_local(&self) -> bool {
        matches!(self.primary_env, PrimaryEnv::Local)
    }
}

/// 中转地址的 scheme。别的 scheme 拨号时才失败，而拨号失败 exec-client 会一直重连，所以解析时就拦住。
fn check_relay_url(url: &str) -> Result<(), String> {
    if url.starts_with("ws://") || url.starts_with("wss://") {
        return Ok(());
    }
    Err(format!(
        "primary_env.relay.url must start with ws:// or wss://, got {url:?}"
    ))
}

fn check_relay_target(user: &str, workdir: &str) -> Result<(), String> {
    if user.trim().is_empty() {
        return Err("primary_env.relay.user is empty".to_owned());
    }
    if !is_absolute(workdir) {
        return Err(format!(
            "primary_env.relay.workdir must be an absolute path in the sandbox, got {workdir:?}"
        ));
    }
    Ok(())
}

/// 解析后的部署：组装只读它。配置文件（[`Deployment`]）与 serve 的握手（`frontends/serve/turn.rs`）各自换成这一个
/// 类型，`assemble::open` 不再分“谁给的”。与配置文件的形状不同的两处：密钥是值（配置文件里只有环境变量的名字），
/// 共用的 PG 连接池是句柄。
///
/// 以前只有配置文件的形状：serve 为了凑它，把握手里的执行票写进一个环境变量、再把变量名填进 `token_env`；PG 的连接池
/// 另放在 `Hosted.pg`，与 `store` 键是同一个决定的两份记录。
#[derive(Clone)]
pub(crate) struct Resolved {
    pub(crate) primary: PrimarySource,
    pub(crate) store: StoreSource,
}

/// 会话的主 env 在哪。
#[derive(Clone)]
pub(crate) enum PrimarySource {
    /// 本进程里的 env-local。
    Local,
    Ssh(SshPrimary),
    Relay(RelaySource),
}

/// 执行中转后面的沙盒：与 [`RelayPrimary`] 相同，只是服务票已经是值。
#[derive(Clone)]
pub(crate) struct RelaySource {
    pub(crate) url: String,
    pub(crate) token: RedactedString,
    pub(crate) user: String,
    pub(crate) workdir: String,
    pub(crate) up_url: Option<String>,
}

impl RelaySource {
    /// 与配置文件同一组规则（地址的 scheme、用户、工作目录）。只有 serve 的握手用它（`cloud` 特性）。
    #[cfg(feature = "cloud")]
    pub(crate) fn validate(&self) -> Result<(), String> {
        check_relay_url(&self.url)?;
        check_relay_target(&self.user, &self.workdir)
    }
}

/// 会话库在哪。
#[derive(Clone)]
pub(crate) enum StoreSource {
    /// 本机 SQLite。
    Local,
    /// 共用的 PG：会话按 `user_id` 隔离（pg-store.md §2）。只在 `cloud` 特性下有：缺省构建不链接 PG 驱动。
    #[cfg(feature = "cloud")]
    Pg { user_id: String, pool: PgPool },
}

/// PG 的连接池从哪来。
#[cfg(feature = "cloud")]
#[derive(Clone)]
pub(crate) enum PgPool {
    /// 这个进程自己开一个：连接串。
    Own(RedactedString),
    /// 宿主开好、所有连接共用的那一个（serve，multi-tenant.md §8.1）。
    Shared(Arc<PgDatabase>),
}

#[cfg(feature = "cloud")]
fn shared_store(pg: &PgStore, env: &EnvSnapshot) -> Result<StoreSource, Fatal> {
    let Some(url) = env.text(&pg.url_env) else {
        return Err(Fatal(format!(
            "Error: shared session database: {} is not set",
            pg.url_env
        )));
    };
    Ok(StoreSource::Pg {
        user_id: pg.user_id.clone(),
        pool: PgPool::Own(RedactedString::new(url.to_owned())),
    })
}

/// 这个构建没有共用的会话库（没开 `cloud` 特性）时配置写了它的错误。
#[cfg(not(feature = "cloud"))]
pub(crate) const NO_SHARED_STORE: &str = "Error: this build has no shared session store: `store.pg` needs zcode-rs built with the `cloud` feature";

/// 缺省构建不带 PG：配置写了共用库就报错退出，不回退本机库（deployment.md §6）。
#[cfg(not(feature = "cloud"))]
fn shared_store(_pg: &PgStore, _env: &EnvSnapshot) -> Result<StoreSource, Fatal> {
    Err(Fatal(NO_SHARED_STORE.to_owned()))
}

/// 由部署推出的开关：算一次，组装的各步只读它。以前每个后果各自再问一次 `primary_is_local()` / `store_is_local()`
/// （9 处），加一种部署形态要逐处检查；现在只改 [`Resolved::traits`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Traits {
    /// 项目级配置文件就在 controller 本机：它的存储键可以改 controller 的存储位置（deployment.md §21）。
    pub(crate) project_config_local: bool,
    /// 持久定时任务可用：任务文件经本机的 fs 读写（§10）。
    pub(crate) durable_cron: bool,
    /// 主 env 上文件类数据的布局（[`super::env_layout::EnvLayout`]）：自动记忆目录与插件数据目录按 controller 的存储路径算，
    /// 内置与已安装的插件可用（同一个文件系统）；否则按主 env 报告的 home，那两种插件不产生（§19.1、§24，ext.md §4.1）。
    pub(crate) memory_on_controller: bool,
    /// 任务工具可用：任务清单只有本机存储，共用会话库下关掉（pg-store.md §6）。
    pub(crate) task_tools: bool,
}

impl Resolved {
    /// 配置文件 → 解析后的部署：`*_env` 指的环境变量在这里读一次，没设就报错退出。
    pub(crate) fn from_config(config: &Deployment, env: &EnvSnapshot) -> Result<Self, Fatal> {
        let primary = match &config.primary_env {
            PrimaryEnv::Local => PrimarySource::Local,
            PrimaryEnv::Ssh(ssh) => PrimarySource::Ssh(ssh.clone()),
            PrimaryEnv::Relay(relay) => {
                let Some(token) = env.text(&relay.token_env) else {
                    return Err(Fatal(format!(
                        "Error: primary_env.relay.token_env names {}, which is not set",
                        relay.token_env
                    )));
                };
                PrimarySource::Relay(RelaySource {
                    url: relay.url.clone(),
                    token: RedactedString::new(token),
                    user: relay.user.clone(),
                    workdir: relay.workdir.clone(),
                    up_url: relay.up_url.clone(),
                })
            }
        };
        let store = match &config.store {
            SessionStoreConfig::Local => StoreSource::Local,
            SessionStoreConfig::Pg(pg) => shared_store(pg, env)?,
        };
        Ok(Self { primary, store })
    }

    /// 主 env 就在本进程里。
    pub(crate) fn primary_is_local(&self) -> bool {
        matches!(self.primary, PrimarySource::Local)
    }

    /// 会话在本机 SQLite 里。
    pub(crate) fn store_is_local(&self) -> bool {
        matches!(self.store, StoreSource::Local)
    }

    pub(crate) fn traits(&self) -> Traits {
        let primary_local = self.primary_is_local();
        Traits {
            project_config_local: primary_local,
            durable_cron: primary_local,
            memory_on_controller: primary_local,
            task_tools: self.store_is_local(),
        }
    }
}

/// 远端的路径风格要连上之后才知道，这里两种绝对路径都认：`/…` 与 `C:\…`、`C:/…`。
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || (bytes.len() > DRIVE_LEN
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[DRIVE_LEN], b'\\' | b'/'))
}

/// 从入口的环境快照读部署配置：没设或为空时是缺省（全本地）。
///
/// 修复（F129，审查 E8）：以前在快照里按原样比较名字，绕过了 Windows 上不分大小写的规则（F58）；现在经 `text`。
pub(crate) async fn load(
    env: &rust_cli_base::secret::EnvSnapshot,
) -> Result<Deployment, DeploymentError> {
    let Some(path) = env.text(VAR) else {
        return Ok(Deployment::default());
    };
    let error = |reason: String| DeploymentError {
        path: path.to_owned(),
        reason,
    };
    let bytes = rust_cli_fs::read_optional(Path::new(path))
        .await
        .map_err(|io| error(io.to_string()))?
        .ok_or_else(|| error("no such file".to_owned()))?;
    let text = String::from_utf8(bytes).map_err(|_| error("not UTF-8".to_owned()))?;
    Deployment::parse(&text).map_err(error)
}

#[cfg(test)]
#[path = "deployment_tests.rs"]
mod tests;
