//! serve 的一轮（docs/specs/rust-cli/multi-tenant.md §4.2）：按握手派生这一条连接的事实与部署配置，照 `-p` 组装
//!（同一组 `assemble::open` / `ports` / `kernel`），跑 headless，输出逐行写进这一轮的 [`TurnLog`]。
//!
//! 与 `-p` 的差别都在组装的输入上，组装与运行本身不分支：home 是这个用户在 serve 数据目录下的目录（会话库、产物、
//! model-io 轨迹、日志都在这里，同运行壳每个用户一个 HOME）；部署配置、出口、PG 连接池与 provider 配置由 serve 给
//! （`assemble::Hosted`）；执行票放进这一轮的环境快照。
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::clock::SystemClock;
use rust_cli_base::names::env;
use rust_cli_base::secret::RedactedString;
use rust_cli_headless::{
    Output, OutputFormat, PrintFormat, PrintOptions, ResultIsError, SessionNotFound, SessionTarget,
    Stamps,
};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_net::Egress;
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_pg_store::PgDatabase;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::protocol::{self, Open, PrimaryEnvSpec, Target, code};
use super::turns::{TurnLog, Turns};
use crate::VERSION;
use crate::assemble::deployment::{PgPool, PrimarySource, RelaySource, Resolved, StoreSource};
use crate::assemble::{self, CliInputs, Controller, Hosted, model_io::ModelIo, models, setup};
use crate::commands::args::{PromptFlags, is_uuid, permission_mode};
use crate::commands::effort;
use crate::process::facts::Facts;

/// headless 与转发任务之间的管道：攒到这么多字节时 headless 等转发取走。
const PIPE_BYTES: usize = 256 * 1024;
/// 用户名直接做目录名的最长长度；更长或带别的字符时用它的摘要。
const MAX_USER_DIR_CHARS: usize = 64;
/// 用户目录名取摘要的前多少个十六进制字符。
const USER_DIGEST_LEN: usize = 32;

/// 进程级的部件：所有连接共用。
pub(crate) struct Shared {
    /// serve 进程的事实；每一轮由它派生（[`plan`]）。
    pub(crate) facts: Facts,
    pub(crate) clock: Arc<SystemClock>,
    pub(crate) egress: Arc<Egress>,
    /// 共用的 PG 连接池；没配时每个用户一个本机库。
    pub(crate) pg: Option<Arc<PgDatabase>>,
    /// 每个用户的目录在 `<data_root>/users/` 下。
    pub(crate) data_root: PathBuf,
    /// 断开后的宽限。
    pub(crate) grace: Duration,
    /// 运行壳的服务凭据（WS 升级请求的 `Authorization: Bearer`）。
    pub(crate) token: RedactedString,
    pub(crate) turns: Turns,
}

/// 按握手定下的一轮：组装的输入与 headless 的选项。
pub(crate) struct Plan {
    facts: Facts,
    spec: Controller,
    paths: StoragePaths,
    target: SessionTarget,
    /// 新开时 `--session-id` 的值。
    new_session: Option<String>,
    partial: bool,
}

/// 一轮里的失败：错误帧的 `code` 与 `message`。
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl Failure {
    fn failed(error: &anyhow::Error) -> Self {
        Self {
            code: code::FAILED,
            message: format!("{error:#}"),
        }
    }
}

/// 握手 → 这一轮的组装输入。不合约定时给出 `bad_request` 的原因。
pub(crate) fn plan(shared: &Shared, open: &Open) -> Result<Plan, String> {
    if open.user.trim().is_empty() {
        return Err("user is empty".to_owned());
    }
    if open.turn_id.trim().is_empty() {
        return Err("turn_id is empty".to_owned());
    }
    let home = shared.data_root.join("users").join(user_dir(&open.user));
    let mut facts = shared.facts.clone();
    let snapshot = facts
        .process
        .env
        .clone()
        .with_var("HOME", &home.to_string_lossy());
    // 握手直接换成解析后的部署（`deployment::Resolved`）：执行票是值，共用的连接池是句柄。以前要先凑成配置文件的
    // 形状——把票写进这一轮的环境快照、再把变量名填进 `token_env`；票因此随环境快照进了 shell、hooks 与 MCP 的输入。
    let primary = match &open.primary_env {
        Some(PrimaryEnvSpec::Relay(relay)) => {
            let token = open
                .exec_token
                .as_deref()
                .filter(|token| !token.is_empty())
                .ok_or("exec_token is required with primary_env.relay")?;
            let relay = RelaySource {
                url: relay.url.clone(),
                token: RedactedString::new(token.to_owned()),
                user: relay.user.clone(),
                workdir: relay.workdir.clone(),
                up_url: relay.up_url.clone(),
            };
            relay.validate()?;
            PrimarySource::Relay(relay)
        }
        // 只给测试：这一轮在 serve 本机这个用户的目录里执行（multi-tenant.md §7）。
        None if facts.hooks.assume.serve_local => PrimarySource::Local,
        None => return Err("primary_env.relay is required".to_owned()),
    };
    let store = match &shared.pg {
        Some(database) => StoreSource::Pg {
            user_id: open.user.clone(),
            pool: PgPool::Shared(database.clone()),
        },
        None => StoreSource::Local,
    };
    let deployment = Resolved { primary, store };
    let provider_config = match &open.provider_config {
        Some(config) if config.is_object() => {
            Some(serde_json::to_vec(config).map_err(|error| error.to_string())?)
        }
        Some(_) => return Err("provider_config must be an object".to_owned()),
        None => None,
    };
    // 给了这一轮的 provider 配置时，serve 进程环境里的显式覆盖（`ZCODE_API_KEY` 等）不生效，同运行壳不传它们。
    if provider_config.is_some() {
        facts.controller.explicit = models::Override::default();
    }
    facts.controller.data_base_dir = None;
    facts.process.home = home.clone();
    facts.process.cwd = home;
    facts.process.env = snapshot;
    facts.process.started = std::time::Instant::now();
    let effort = open.effort.as_deref().map(effort::parse).transpose()?;
    let mode = match open.permission_mode.as_deref() {
        Some(value) => {
            Some(permission_mode(value).ok_or(format!("unknown permission_mode {value:?}"))?)
        }
        None => None,
    };
    let (target, new_session) = match &open.session {
        Target::New(uuid) if is_uuid(uuid) => (SessionTarget::New, Some(uuid.clone())),
        Target::New(_) => return Err("session.new must be a UUID".to_owned()),
        Target::Resume(id) if !id.trim().is_empty() => {
            (SessionTarget::Resume(SessionId::new(id.clone())), None)
        }
        Target::Resume(_) => return Err("session.resume is empty".to_owned()),
    };
    let cli = CliInputs {
        model: open.model.clone(),
        effort,
        permission_mode: mode,
        disallowed: open.disallowed_tools.clone(),
        max_turns: open.max_turns,
        append_system_prompt: PromptFlags {
            inline: open.append_system_prompt.clone(),
            file: None,
        },
        ..CliInputs::default()
    };
    let hosted = Hosted {
        deployment,
        egress: shared.egress.clone(),
        provider_config,
    };
    let paths = facts.storage_paths(&StorageSettings::default());
    Ok(Plan {
        facts,
        spec: Controller::serve(cli, hosted),
        paths,
        target,
        new_session,
        partial: open.partial_messages,
    })
}

/// 跑这一轮，输出写进 `log`；失败时最后一行是错误帧。跑完后 `log` 记为跑完；宽限已到（没人会来取）时从表里拿掉。
pub(crate) async fn run(shared: Arc<Shared>, plan: Plan, prompt: String, log: Arc<TurnLog>) {
    let model_io = ModelIo::default();
    let turn_id = log.key().turn_id.clone();
    if let Err(failure) = turn(&shared, plan, prompt, &log, &model_io).await {
        tracing::warn!(event = "serve.turn.failed", code = failure.code, error = %failure.message);
        log.push(|seq| {
            protocol::tag(
                &protocol::error_frame(failure.code, &failure.message),
                &turn_id,
                seq,
            )
        });
    }
    model_io.flush().await;
    if log.finish() {
        shared.turns.end(log.key());
    }
}

async fn turn(
    shared: &Shared,
    plan: Plan,
    prompt: String,
    log: &Arc<TurnLog>,
    model_io: &ModelIo,
) -> Result<(), Failure> {
    let Plan {
        facts,
        spec,
        paths,
        target,
        new_session,
        partial,
    } = plan;
    ensure_dir(&facts.process.home).await?;
    let clock = &shared.clock;
    let opened = assemble::open(&spec, &facts, (&paths, clock))
        .await
        .map_err(|error| Failure::failed(&error))?;
    let session_id = match new_session {
        Some(uuid) => Some(
            setup::fresh_session_id(opened.store.as_ref(), &uuid)
                .await
                .map_err(|error| Failure {
                    code: code::SESSION_EXISTS,
                    message: format!("{error:#}"),
                })?,
        ),
        None => None,
    };
    let ported = assemble::ports(&spec, &facts, opened, (clock, model_io))
        .await
        .map_err(|error| Failure::failed(&error))?;
    let built = assemble::kernel(&spec, &facts, ported, clock)
        .await
        .map_err(|error| Failure::failed(&error))?;
    tokio::select! {
        () = built.kernel.wait_for_extensions() => {}
        () = log.stop.cancelled() => {}
    }
    let output = Output {
        format: PrintFormat {
            output: OutputFormat::StreamJson,
            verbose: true,
            partial,
        },
        stamps: Some(Stamps {
            ids: built.opened.ids.clone(),
            clock: clock.clone(),
        }),
        version: VERSION.to_owned(),
        api_key_source: if built.opened.directory.is_none() {
            env::ZCODE_API_KEY
        } else {
            "none"
        }
        .to_owned(),
    };
    let options = PrintOptions {
        prompt,
        session_id,
        workspace: built.opened.workspace.clone(),
        selection: built.selection.current(),
        mode: built.mode,
        target,
        fork: false,
        stop: log.stop.clone(),
        cancel: log.cancel.clone(),
        output,
        ids: built.opened.ids.clone(),
    };
    let (writer, reader) = tokio::io::duplex(PIPE_BYTES);
    let kernel = built.kernel.clone();
    let printing = async move {
        let mut writer = writer;
        // 写完（或失败）后丢掉写端，转发那一侧读到结尾。
        rust_cli_headless::run(kernel, options, &mut writer).await
    };
    let (printed, ()) = tokio::join!(printing, forward(reader, log));
    drop(built);
    match printed {
        Ok(()) => Ok(()),
        // 最后一个 result 是错误：输出已经写好（`-p` 只按退出码 1 结束），这一轮照常结束。
        Err(error) if error.downcast_ref::<ResultIsError>().is_some() => Ok(()),
        Err(error) => match error.downcast_ref::<SessionNotFound>() {
            Some(missing) => Err(Failure {
                code: code::SESSION_NOT_FOUND,
                message: missing.to_string(),
            }),
            None => Err(Failure::failed(&error)),
        },
    }
}

/// headless 写出的行逐行加上 `turn_id` 与 `seq`，进这一轮的缓冲。
async fn forward(reader: tokio::io::DuplexStream, log: &Arc<TurnLog>) {
    let turn_id = log.key().turn_id.clone();
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => log.push(|seq| protocol::tag(&line, &turn_id, seq)),
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(event = "serve.turn.read_failed", %error);
                break;
            }
        }
    }
}

async fn ensure_dir(path: &Path) -> Result<(), Failure> {
    rust_cli_fs::create_dir_all(path)
        .await
        .map_err(|error| Failure {
            code: code::FAILED,
            message: format!("creating {}: {error}", path.display()),
        })
}

/// 用户在 serve 数据目录下的目录名：只含字母、数字、`.`、`_`、`-` 且不长时用它本身，否则用它的摘要（不让用户名拼出
/// 别的路径）。
pub(crate) fn user_dir(user: &str) -> String {
    let plain = !user.is_empty()
        && user.len() <= MAX_USER_DIR_CHARS
        && user != "."
        && user != ".."
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if plain {
        return user.to_owned();
    }
    let digest = format!("{:x}", Sha256::digest(user.as_bytes()));
    format!("u-{}", &digest[..USER_DIGEST_LEN])
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
