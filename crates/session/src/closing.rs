//! 关闭会话（docs/specs/rust-cli/session-lifecycle.md §3.3、§3.3.1）：只关一次，关闭任务在内核的根作用域里跑完。
//!
//! 修复（F33）：以前关闭在调用方的任务里跑，第二次 `close` 在 actor 还在时再跑一次 SessionEnd（并让第一次的
//! `Request::Close` 失去回复）；调用方丢下 future 时会话停在半关的状态。会话表加了“关闭中”之后，打开同一个会话要
//! 等关闭走完，所以关闭必须不依赖调用方：第一次 `close` 起关闭任务，之后的调用等同一个任务结束。

use rust_cli_base::sync::LockUnpoisoned;

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::ids::{MachineName, SessionId};
use rust_cli_kernel_api::session_api::{EndReason, KernelError};
use rust_cli_kernel_api::store::SessionMeta;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::actor::Request;
use crate::{Session, SessionDeps, hooks};

/// 发一个请求给 actor、等回复；actor 已经退出时为 `closed`。
pub(crate) async fn ask<T>(
    requests: &mpsc::Sender<Request>,
    request: impl FnOnce(oneshot::Sender<T>) -> Request,
) -> Result<T, KernelError> {
    let (reply, answer) = oneshot::channel();
    requests
        .send(request(reply))
        .await
        .map_err(|_| crate::closed())?;
    answer.await.map_err(|_| crate::closed())
}

/// 占关闭的闩（只关一次，§3.3.1）：没人关过就占上、返回自己的令牌；已经关完（令牌已取消）为 None；有人在关时等它，醒来再看一次——
/// `close_idle` 判出不空闲时放开闩（§3.3.2），醒来的调用方自己再占。闩的进度见 `Session::closing`。
async fn latch(session: &Session) -> Option<CancellationToken> {
    loop {
        let running = {
            let mut latch = session.closing.lock_unpoisoned();
            match latch.as_ref() {
                None => {
                    let done = CancellationToken::new();
                    *latch = Some(done.clone());
                    return Some(done);
                }
                Some(running) if running.is_cancelled() => return None,
                Some(running) => running.clone(),
            }
        };
        running.cancelled().await;
    }
}

/// 关闭任务在内核的根作用域里跑，不依赖调用方（§3.3.1）；内核正在释放（根作用域已关闭）时在调用方的任务里跑完。
async fn finish<F>(session: &Session, work: F) -> Vec<String>
where
    F: std::future::Future<Output = Vec<String>> + Send + 'static,
{
    let spawner = &session.deps.closing;
    if spawner.is_closed() {
        return work.await;
    }
    spawner
        .spawn("session-close", work)
        .await
        .unwrap_or_default()
}

/// `SessionHandle::close`：第一次起关闭任务并等它的结果；关闭中又来关闭（或已经关完）时等同一个任务结束，返回
/// 空的提示——SessionEnd 只跑一次，提示归第一次的调用方。
pub(crate) async fn close(session: &Session, reason: EndReason) -> Vec<String> {
    let Some(done) = latch(session).await else {
        return Vec::new();
    };
    // 会话表：Open → Closing，早于 SessionEnd，并且就在这一次轮询里（不等关闭任务被调度）：从这时起打开同一个会话
    // 都等它关完（§3.3.1）。
    session.deps.released.closing(&session.id);
    let work = run(
        session.id.clone(),
        session.requests.clone(),
        session.deps.clone(),
        reason,
        // 守卫随关闭任务的 future 一起丢弃：任务结束、被中止、一次都没跑，等它的 `close` 都会醒来。
        done.drop_guard(),
    );
    finish(session, work).await
}

/// `SessionHandle::close_idle`（§3.3.2）：先标关闭中（判完空闲之后打开同一个会话的都等，F33），再让 actor 在它的一步里判空闲并进入关闭；
/// 不空闲时会话表换回打开着、放开闩，返回 false。已经关完或别人关完时返回 true。
pub(crate) async fn close_idle(session: &Session, reason: EndReason) -> bool {
    let Some(done) = latch(session).await else {
        return true;
    };
    session.deps.released.closing(&session.id);
    let (closed, exited) = oneshot::channel();
    let meta = match ask(&session.requests, |reply| Request::CloseIdle {
        reply,
        closed,
    })
    .await
    {
        Ok(Some(meta)) => Some(meta),
        Ok(None) => {
            // 先换回会话表、再放开闩：醒来的 `close` 再占闩时看到的是打开着的条目，照常标关闭中
            session.deps.released.kept(&session.id);
            *session.closing.lock_unpoisoned() = None;
            done.cancel();
            return false;
        }
        // actor 已经没了（例如 panic）：同 `close`，不跑 SessionEnd，照常释放
        Err(_) => None,
    };
    let work = idle_run(
        session.id.clone(),
        session.deps.clone(),
        reason,
        meta,
        exited,
        done.drop_guard(),
    );
    finish(session, work).await;
    true
}

/// 关闭任务（spec §3.3 的关闭路径）。每一步失败都照常往下走：最后的 `released` 总会把会话移出会话表。
async fn run(
    id: SessionId,
    requests: mpsc::Sender<Request>,
    deps: SessionDeps,
    reason: EndReason,
    _done: DropGuard,
) -> Vec<String> {
    tracing::info!(event = "session.close", session = %id, reason = ?reason, "session closed");
    let notices = match ask(&requests, |reply| Request::Meta { reply }).await {
        Ok(meta) => hooks::session_end(&deps, &meta, reason).await,
        // actor 已经没了（例如 panic）：没有 meta，不跑 SessionEnd，照常释放。
        Err(_) => Vec::new(),
    };
    // actor 已经没了时这一步失败；同样照常往下走。退出（`prompt_input_exit`、`other`）不再记录在跑的回合
    // （`Shutdown`，修复线 F38，spec §3.5）：`/exit` 与退出信号不中止 query、直接退出。
    let abort = match reason {
        EndReason::Clear | EndReason::Resume => AbortReason::User,
        EndReason::Logout | EndReason::PromptInputExit | EndReason::Other => AbortReason::Shutdown,
    };
    let _ = ask(&requests, |reply| Request::Close {
        reply,
        reason: abort,
    })
    .await;
    release(&id, &deps, reason).await;
    notices
}

/// 空闲时关闭的关闭任务（§3.3.2）：actor 已经进入关闭（不开新工作），SessionEnd 只读 meta，跑在它退出之前或之后结果相同。
async fn idle_run(
    id: SessionId,
    deps: SessionDeps,
    reason: EndReason,
    meta: Option<SessionMeta>,
    exited: oneshot::Receiver<()>,
    _done: DropGuard,
) -> Vec<String> {
    tracing::info!(event = "session.close", session = %id, reason = ?reason, idle = true, "session closed");
    let notices = match meta {
        Some(meta) => hooks::session_end(&deps, &meta, reason).await,
        None => Vec::new(),
    };
    // actor 已经没了时立刻返回
    let _ = exited.await;
    release(&id, &deps, reason).await;
    notices
}

/// 关闭的后半段（`close` 与 `close_idle` 同一份）：actor 退出之后释放执行环境、远端机器与定时任务，最后交组装方。
async fn release(id: &SessionId, deps: &SessionDeps, reason: EndReason) {
    // 后台任务跨 `/clear`、`/resume` 保留（只停前台任务）：这两种关闭
    // 不让执行环境忘掉会话（后台命令继续跑、通知留着）；退出程序时全部释放。
    let forget = !matches!(reason, EndReason::Clear | EndReason::Resume);
    let primary = deps.machines.primary_machine();
    if let Err(error) = primary.commands().close_session(id, forget).await {
        tracing::warn!(%error, session = %id, "the environment could not close the session");
    }
    // 连上的远端机器同样关闭（R3，remote-exec.md §20.7.2）：没连上的那台以后重连时不会再送这个会话的通知给它。
    for remote in deps.machines.machines().machines {
        let name = MachineName::new(remote.name);
        let Some(machine) = deps.machines.machine(&name) else {
            continue;
        };
        // 远端执行 F96（§23.3）：重连中的机器不等（SSH 一直重连，等它会挡住关闭）；请求交给关闭任务的作用域在后台发，
        // 连回来时送达，进程退出时随作用域结束。
        if deps.machines.reconnecting(&name) {
            let session = id.clone();
            drop(deps.closing.spawn("close-remote-session", async move {
                if let Err(error) = machine.commands().close_session(&session, forget).await {
                    tracing::warn!(%error, %session, machine = %name, "a remote machine could not close the session");
                }
            }));
            continue;
        }
        if let Err(error) = machine.commands().close_session(id, forget).await {
            tracing::warn!(%error, session = %id, machine = %name, "a remote machine could not close the session");
        }
    }
    // 停止这个会话的持久定时任务调度、释放调度锁（tools-more.md §16.4）。
    deps.cron.session_closed(id).await;
    // 最后一步：组装方清按会话的表、把会话移出会话表（等它的打开在这之后醒来）。
    deps.released.released(id);
}

impl crate::actor::Actor {
    /// 关闭（spec §3.3，`/clear`、`/resume` 的前台任务处理）：中止在跑的回合与可取消的后台工作，取消待回答的
    /// 交互；排队的输入与通知不再开回合（通知没有 ack，留在执行环境与 agents 里，会话再打开时送达）。
    pub(crate) async fn close(&mut self, reply: oneshot::Sender<()>, reason: AbortReason) {
        tracing::info!(event = "session.closing", session = %self.meta.id, busy = self.busy(), "session closing");
        self.closing = Some(reply);
        // `/clear`、`/resume`（用户中止）之后旧会话的后台子代理照样询问（agents.md §4.1.2，工作线 F72）：先登记这个会话
        // 与它的标题，下面取消的会话级询问与之后的询问都转到进程级的入口。退出程序时不登记。
        if reason == AbortReason::User
            && let Some(detached) = &self.deps.detached
        {
            detached.closed(&self.meta.id, &self.meta.title);
        }
        if let Some(running) = &self.running {
            running.abort.abort(reason);
        }
        if let Some(cancel) = self.job.as_ref().and_then(|job| job.cancel.as_ref()) {
            cancel.cancel();
        }
        self.cancel_pending();
    }
}
