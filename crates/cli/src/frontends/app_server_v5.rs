//! `zcode app-server --stdio --protocol v5`（docs/specs/rust-cli/protocol-v5.md §2、§4）：ZCode Protocol v5 的入口。
//!
//! 组装与 v4 相同（`Controller::app`、`assemble::open`、`assemble::ports`、`WorkspaceKernels`，F145 的进程级与工作区级），区别：
//! - 传输：Content-Length 分帧、JSON-RPC 2.0；读循环按到达顺序把消息交给 protocol-v5 的 actor（它是连接状态的唯一 owner）；
//! - 读循环先起：握手不等组装；存储的启动事实与“进程级部件建好”经句柄报给 actor（进程级 `agent` 的 `storage` 与 `phase`）；
//! - 进程 cwd 那一套内核不在启动时组装：v5 没有不带工作区的会话请求，工作区第一次出现时再组装；
//! - 账号模型的请求头来源是 protocol-v5 的 `HostCredentials`（经 CLI→Host 的 `service` 请求问 Host，protocol-v5.md §30）；
//! - 账号 provider 的权益、当前连接与模型白名单来自 Host 推的外部账号层（`account.overlay.set`，protocol-v5.md §36、platform.md §12）；
//! - 远程 MCP 服务器的令牌经 Host 加解密（`host.credentials.cipher`，protocol-v5.md §74、ext.md §3.12.9）。

use std::sync::Arc;

use anyhow::Result;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::store::{ArtifactStore, SessionFilter, StoreQueries};
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_core::framing::{Dialect, Framing};
use rust_cli_protocol_v5::{Handle, MAX_MESSAGE_BYTES, Settings, Storage};

use super::app_server::{Parsed, SCOPE_CLOSE_GRACE, database_id};
use crate::VERSION;
use crate::assemble::workspaces::WorkspaceKernels;
use crate::assemble::{self, Controller};
use crate::frontends::app::AppFrontend;
use crate::frontends::app_plugins::AppPlugins;
use crate::frontends::app_workspaces::V5Workspaces;
use crate::plugin_admin::PluginAdmin;
use crate::process::{ControllerRun, Start};
use rust_cli_ext::McpClientTraits;

/// 存储打不开时报的错误码（存储启动错误码的词表，同 v4 的 `startup/storageState`）。
const STORAGE_OPEN_FAILED: &str = "open_failed";
/// v5 的界面撑得起的 MCP 服务器询问（ext.md §3.13.1、protocol-v5.md §76）：表单与 sampling 的确认经 v5 的交互画；网址模式要界面能
/// 打开浏览器、等完成，没做。
const MCP_CLIENT: McpClientTraits = McpClientTraits {
    elicitation_form: true,
    elicitation_url: false,
    sampling: true,
};

pub(crate) async fn run(parsed: Parsed) -> Result<u8> {
    // 日志只写文件：stdout 是协议（logging.md §6）。
    let run = ControllerRun::begin(Start {
        mode: "app-server",
        surface: Some(parsed.surface.as_deref().unwrap_or("none")),
    })?;
    // 声明的前端能力（context.md §15.5）：Host 的错（声明了 gen-ui 却没给目录）在握手之前结束进程，Host 按拉起失败回退
    let frontend = match AppFrontend::declare(&parsed.frontend, &run.facts.process.env) {
        Ok(frontend) => frontend,
        Err(error) => return run.finish(Err(anyhow::anyhow!(error))).await,
    };
    let scope = TaskScope::root("app-server", |panic| {
        tracing::error!(event = "task.panic", scope = %panic.scope, task = panic.task, message = %panic.message, "a task panicked");
    });
    let options = Options {
        framing: Framing::ContentLength,
        dialect: Dialect::JsonRpc2,
        max_message_bytes: MAX_MESSAGE_BYTES,
        ..Options::default()
    };
    let (peer, writer) = connection::start(tokio::io::stdout(), &scope.spawner(), options);
    // 插件管理的端口（protocol-v5.md §72.7）：握手时就声明 catalog.plugins；服务与内核缓存在组装完成时交给它
    let plugins = Arc::new(AppPlugins::default());
    let settings = Settings {
        version: VERSION.to_owned(),
        plugins: Some(plugins.clone()),
        // MCP 的授权（protocol-v5.md §74）：组装总装上（令牌经 Host 加解密，`Controller::with_mcp_cipher`）
        mcp_oauth: true,
    };
    let (handle, intake, actor) =
        rust_cli_protocol_v5::start(peer.clone(), settings, &scope.spawner());
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    let result = {
        let reading = connection::serve_in_order(input, peer, intake, options);
        let assembly = assemble_into(&run, &handle, (frontend, &plugins));
        tokio::pin!(reading, assembly);
        let mut assembled = false;
        // 读循环与组装同时进行：握手不等组装。对端关掉输入（或帧坏了）、收到退出信号时结束（Host 回收整棵进程树）。
        // 组装是循环里的一个分支，完成后不再选它（不能在 select! 的分支体里等：组装先完成时读循环会被丢掉）。
        loop {
            tokio::select! {
                result = &mut reading => break result.map_err(anyhow::Error::from),
                () = run.stop.token.cancelled() => break Ok(()),
                // 组装失败只报给 actor（agent 的 phase），读循环照常
                () = &mut assembly, if !assembled => assembled = true,
            }
        }
    };
    drop(handle);
    drop(actor);
    // 写者写完队列里剩下的帧再退出。
    drop(writer);
    let _ = scope.close(SCOPE_CLOSE_GRACE).await;
    run.finish(result.map(|()| 0)).await
}

/// 组装进程级部件，把存储的启动事实与“进程级部件建好”报给 actor。失败时只记日志、报 degraded，不结束进程（Host 能读到
/// 原因，再决定重启，protocol-v5.md §12 第 2 项）。
async fn assemble_into(
    run: &ControllerRun,
    handle: &Handle,
    parts: (
        (
            rust_cli_kernel_api::session_prompt::FrontendTraits,
            AppFrontend,
        ),
        &AppPlugins,
    ),
) {
    if let Err(error) = assemble(run, handle, parts).await {
        tracing::warn!(event = "app.assemble.failed", error = %error, "assembling the process-level parts failed");
    }
}

async fn assemble(
    run: &ControllerRun,
    handle: &Handle,
    (frontend, plugins): (
        (
            rust_cli_kernel_api::session_prompt::FrontendTraits,
            AppFrontend,
        ),
        &AppPlugins,
    ),
) -> Result<()> {
    let ControllerRun {
        facts,
        paths,
        clock,
        model_io,
        ..
    } = run;
    // 插件管理的服务按 App 声明的前端能力判哪些插件撑不起来（plugin-store.md §12）
    let unhosted = frontend.1.unhosted();
    // 账号模型的凭据经 Host 取（protocol-v5.md §30）：Host 没告知这个服务时来源直接报鉴权失败。账号层也以 Host 为准（§36）
    let (traits, app) = frontend;
    // 远程 MCP 服务器的令牌经 Host 加解密（ext.md §3.12.9）：桌面上系统凭据库由 Main 独占
    let spec = Controller::app(
        handle.credentials(clock.clone()),
        Some(handle.accounts()),
        (traits, app, MCP_CLIENT),
    )
    .with_mcp_cipher(handle.cipher());
    let opened = match assemble::open(&spec, facts, (paths, clock)).await {
        Ok(opened) => opened,
        Err(error) => {
            handle.storage(Storage::Failed {
                code: STORAGE_OPEN_FAILED.to_owned(),
            });
            return Err(error);
        }
    };
    handle.storage(Storage::Checking {
        database_id: database_id(opened.paths.database().as_path()),
    });
    // 会话库：打开一次（node-store 在打开连接时校验迁移）
    if let Err(error) = opened
        .store
        .list(SessionFilter {
            workspace: opened.workspace.clone(),
            limit: 1,
        })
        .await
    {
        handle.storage(Storage::Failed {
            code: STORAGE_OPEN_FAILED.to_owned(),
        });
        anyhow::bail!("opening the session database failed: {error}");
    }
    handle.storage(Storage::Ready);
    // 会话列表的事实与变化（protocol-v5.md §14）：进程级的一份会话库，全部工作区共用（F145）；与工作区内核里的是同一个 NodeStore
    let queries: Arc<dyn StoreQueries> = opened.store.clone();
    // 上传的附件写进会话的 Artifact、读面读它（protocol-v5.md §39）：同一个 NodeStore。产物总在本机存储（`Opened::node`，
    // pg-store.md §6）：会话库可以是别的实现，产物不跟着它走。
    let artifacts: Arc<dyn ArtifactStore> = opened.node.clone();
    // 新会话的缺省在建会话那一刻按目录取（账号层后来变了也跟着变，22 §3.5 第 6 项）；显式覆盖没有目录
    let directory = opened.directory.clone();
    // 插件管理的服务：本机文件、进程的同一个出口（plugin-store.md §9.4）、App 声明的前端能力（列出时按它判加没加载）
    let admin = PluginAdmin::new(
        (opened.env.clone(), facts.process.home.clone()),
        paths.clone(),
        facts.process.env.clone(),
        opened.egress.clone(),
        (clock.clone(), unhosted),
    );
    let ported = assemble::ports(&spec, facts, opened, (clock, model_io)).await?;
    // v5 的 App 扣住停止或失败之后的队列（02 §11 C2，protocol-v5.md §15.5）；v4 的组装不经这里
    let kernels = WorkspaceKernels::new(&spec, facts, (paths, clock), ported).holding_queue();
    let workspaces = Arc::new(V5Workspaces::new(kernels, directory));
    // 插件改了之后拿走受影响的内核（24 §13.2）：与会话命令用同一份缓存
    plugins.ready(admin, workspaces.cache());
    handle.ready(workspaces, queries, artifacts);
    Ok(())
}
