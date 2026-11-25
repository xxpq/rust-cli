//! `zcode app-server --stdio [--surface desktop] [--protocol v4|v5] [--frontend <能力,…>]`（docs/specs/rust-cli/app-protocol.md
//! §3.7、§10；protocol-v5.md §2）：App 的 Host 经 stdio 拉起的协议前端。参数照 Host 现在给 Node 的
//! （`ZA:zcodeAgentProcessManager.ts` 的 `applyPresentationSurfaceToCommand`）；`--protocol v5` 进 v5 的前端（[`super::app_server_v5`]），
//! 缺省 v4，与以前逐字相同。
//!
//! 组装与 `-p`、交互界面共用（[`crate::assemble`]，规格是 `Controller::app`：交互形态的提示词与工具；不问工作区信任、生成
//! 标题、不连远端机器、不登记跨会话通信、只接受本地主 env、root 下的 bypass 照 Node 不拒绝）；区别只在注入的东西：平台层的
//! 账号凭据来源经 Host（G8），前端是 `protocol-zcode`。写者先起；读完配置、库的位置定了之后报 `startup/storageState` 的
//! `checking`（Host 要求拉起后 30 s 内有首帧），接着查会话库（打不开时报 `failed`），建好内核、开始连接 MCP 之后报 `ready`。
//! 一个进程服务所有本地工作区（工作线 F145）：每个工作区一套内核（`assemble::workspaces`），进程 cwd 的那一套在 `ready`
//! 之前建好。

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::store::SessionFilter;
use rust_cli_node_store::LOCAL_ENVIRONMENT;
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_zcode::{Context, Defaults, HostHeaders, Startup};

use crate::assemble::workspaces::WorkspaceKernels;
use crate::assemble::{self, Controller};
use crate::frontends::app::AppFrontend;
use crate::frontends::app_workspaces::HostedWorkspaces;
use crate::process::{ControllerRun, Start};
use rust_cli_ext::McpClientTraits;

/// 退出时等进程级任务收尾的上限（存活的上限，不用来同步状态）。
pub(crate) const SCOPE_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// 协议（`--protocol`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Protocol {
    /// 以前的路径（protocol-zcode，冻结在 P1 子集）。
    #[default]
    V4,
    /// ZCode Protocol v5（protocol-v5，F141）。
    V5,
}

/// `--protocol` 的取值。
const PROTOCOL_V4: &str = "v4";
const PROTOCOL_V5: &str = "v5";

/// 子命令的参数。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Parsed {
    /// `--surface`（Host 给 `desktop`）：只记日志，不改提示词（提示词按 `--frontend` 声明的能力，context.md §15.5）。
    pub(crate) surface: Option<String>,
    pub(crate) protocol: Protocol,
    /// `--frontend`（context.md §15.5）：Host 声明的前端能力；没给时全关。
    pub(crate) frontend: Vec<super::app::Declared>,
}

/// 一个带值的选项：`--name value` 或 `--name=value`。
fn option_value<'a>(
    arg: &str,
    name: &str,
    rest: &mut impl Iterator<Item = &'a String>,
) -> Option<Result<String, String>> {
    if arg == name {
        let label = name.trim_start_matches('-');
        return Some(
            rest.next()
                .cloned()
                .ok_or_else(|| format!("error: option '{name} <{label}>' argument missing")),
        );
    }
    arg.strip_prefix(name)
        .and_then(|tail| tail.strip_prefix('='))
        .map(|value| Ok(value.to_owned()))
}

/// `app-server` 子命令；不是它时为 None。认 `--stdio`、`--surface`、`--protocol`、`--frontend`。
/// 以前的 `--expect-build` 删了（2026-11-05）：两端是否同包改由握手里的契约指纹判定（docs/zcode-protocol-v5/03 §3.7）。
pub(crate) fn parse(args: &[String]) -> Option<Result<Parsed, String>> {
    let (first, rest) = args.split_first()?;
    if first != "app-server" {
        return None;
    }
    let mut stdio = false;
    let mut parsed = Parsed::default();
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        if arg == "--stdio" {
            stdio = true;
        } else if let Some(value) = option_value(arg, "--surface", &mut rest) {
            match value {
                Ok(value) => parsed.surface = Some(value),
                Err(error) => return Some(Err(error)),
            }
        } else if let Some(value) = option_value(arg, super::app::FRONTEND_FLAG, &mut rest) {
            match value.and_then(|value| super::app::parse(&value)) {
                Ok(declared) => parsed.frontend = declared,
                Err(error) => return Some(Err(error)),
            }
        } else if let Some(value) = option_value(arg, "--protocol", &mut rest) {
            parsed.protocol = match value.as_deref() {
                Ok(PROTOCOL_V4) => Protocol::V4,
                Ok(PROTOCOL_V5) => Protocol::V5,
                Ok(other) => {
                    return Some(Err(format!(
                        "error: option '--protocol <protocol>' argument '{other}' is invalid. Allowed choices are {PROTOCOL_V4}, {PROTOCOL_V5}."
                    )));
                }
                Err(error) => return Some(Err(error.clone())),
            };
        } else {
            return Some(Err(format!("error: unknown option '{arg}'")));
        }
    }
    if !stdio {
        return Some(Err("error: app-server needs --stdio".to_owned()));
    }
    Some(Ok(parsed))
}

/// 会话库的不透明 ID：库路径的散列（固定的键，同一个库总是同一个 ID；不带路径）。
pub(crate) fn database_id(path: &std::path::Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("session-db-{:016x}", hasher.finish())
}

pub(crate) async fn run(parsed: Parsed) -> Result<u8> {
    if parsed.protocol == Protocol::V5 {
        return super::app_server_v5::run(parsed).await;
    }
    // 日志只写文件：stdout 是协议（logging.md §6）。
    let run = ControllerRun::begin(Start {
        mode: "app-server",
        surface: Some(parsed.surface.as_deref().unwrap_or("none")),
    })?;
    // 声明的前端能力（context.md §15.5）：Host 的错（声明了 gen-ui 却没给目录）在这里结束进程，不进协议
    let frontend = match AppFrontend::declare(&parsed.frontend, &run.facts.process.env) {
        Ok(frontend) => frontend,
        Err(error) => return run.finish(Err(anyhow::anyhow!(error))).await,
    };
    let scope = TaskScope::root("app-server", |panic| {
        tracing::error!(event = "task.panic", scope = %panic.scope, task = panic.task, message = %panic.message, "a task panicked");
    });
    let (peer, writer) =
        connection::start(tokio::io::stdout(), &scope.spawner(), Options::default());
    let context = Arc::new(Context::new(peer, run.clock.clone()));
    let result = serve(&run, context, scope.spawner(), frontend).await;
    // 写者写完队列里剩下的帧再退出。
    drop(writer);
    let _ = scope.close(SCOPE_CLOSE_GRACE).await;
    run.finish(result.map(|()| 0)).await
}

async fn serve(
    run: &ControllerRun,
    context: Arc<Context>,
    spawner: rust_cli_base::Spawner,
    frontend: (
        rust_cli_kernel_api::session_prompt::FrontendTraits,
        AppFrontend,
    ),
) -> Result<()> {
    let ControllerRun {
        facts,
        paths,
        clock,
        stop,
        model_io,
        ..
    } = run;
    // v4 冻结：MCP 服务器的询问不声明（ext.md §3.13.1），服务器的请求照旧回 Method not found
    let (traits, app) = frontend;
    let spec = Controller::app(
        Arc::new(HostHeaders::new(context.clone())),
        None,
        (traits, app, McpClientTraits::default()),
    );
    let opened = assemble::open(&spec, facts, (paths, clock)).await?;
    // 修复（F133，modularity.md §7 的 P5）：以前 ID 取读配置之前的缺省库路径、在开端口之前就报 `checking`，配置或环境变量
    // 改了 `storage.sessionDbPath` 时 ID 不是实际打开的库。ID 又不能在首帧之后改（Host 丢掉 ID 变了的帧，app-protocol.md
    // §4.2），所以首帧等库的位置定了再发：ID 取会话库所用的同一份存储位置。
    let state = Startup::new(
        context.clone(),
        database_id(opened.paths.database().as_path()),
    );
    state.checking().await;
    // 会话库：打开一次（node-store 在打开连接时校验迁移）；失败时报 failed，不再往下组装。
    if let Err(error) = opened
        .store
        .list(SessionFilter {
            workspace: opened.workspace.clone(),
            limit: 1,
        })
        .await
    {
        tracing::warn!(event = "app.storage.failed", error = %error, "opening the session database failed");
        state.failed("open_failed").await;
        anyhow::bail!("opening the session database failed: {error}");
    }
    let cwd = opened.workspace.root.as_str().to_owned();
    let ported = assemble::ports(&spec, facts, opened, (clock, model_io)).await?;
    // 一个进程服务所有本地工作区（F145，app-protocol.md §15）：进程级的部件建一次，每个工作区一套内核；进程 cwd 的那一套
    // 在 ready 之前组装（与以前建好内核再报 ready 相同）。
    let kernels = WorkspaceKernels::new(&spec, facts, (paths, clock), ported);
    let selection = kernels.selection();
    let workspaces = Arc::new(HostedWorkspaces::start(kernels, &cwd).await);
    state.ready().await;
    let defaults = Defaults {
        selection,
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
    };
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    // 读到 Host 关掉输入或收到退出信号时结束（Host 回收整棵进程树）。
    tokio::select! {
        result = rust_cli_protocol_zcode::serve(input, context, workspaces, defaults, spawner, Options::default()) => {
            result?;
        }
        () = stop.token.cancelled() => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "app_server_tests.rs"]
mod tests;
