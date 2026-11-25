//! 进程级的授权登记（docs/specs/rust-cli/ext.md §3.12.14）：一个 rust 进程里所有内核、所有会话级的管理器共用一份。
//!
//! - 进行中的授权：每个授权目标最多一个；用户再发起就丢下旧的（关掉它的监听），授权伪工具 180 s 内接着用自己开始的那一个。
//! - 授权的代号：授权完成、退出授权时加一。连接任务开始时记下，结果是要授权而代号已经变了时直接再连一次（§3.12.14 的竞态表）。
//! - 授权变了的通知：直接调每个活着的管理器（弱引用；会话释放、内核丢掉之后跳过并清掉），不用计时器、不经广播。
//!
//! 令牌不在这里（授权端口背后的凭据文件）；连接状态的唯一 owner 仍是每个管理器的连接任务。

use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures_util::future::join_all;
use rust_cli_base::Spawner;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::manage::McpSignInOutcome;
use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpAuthorizer, PastedCallback};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::{Inner, McpManager};

/// 这个内核没有授权登记（入口不装它：v4 的 App、serve、测试）。
pub(crate) const NO_AUTHORIZER: &str = "Signing in to MCP servers is not available here";
/// 这一次授权被后来的一次替换、或随内核的作用域结束了。
pub(crate) const SIGN_IN_ENDED: &str = "The sign-in was replaced by a newer one or ended";
/// 授权伪工具接着用自己开始的那一次的期限（§3.12.16）。
const TOOL_REUSE: Duration = Duration::from_secs(180);

/// 授权变成了什么（§3.12.14）：管理器按它挑要重连的连接。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthChange {
    SignedIn,
    SignedOut,
}

/// 开始了（或接着用）的一次授权。
pub(crate) struct Started {
    pub url: String,
    /// 是伪工具接着用的、早先开始的那一次（authenticate 的结果前面多一段，§3.12.16）。
    pub reused: bool,
    pub outcome: Outcome,
}

/// 贴回来的回调地址交给进行中的授权（complete_authentication，§3.12.16）的结果。
pub(crate) enum Paste {
    /// 这个目标没有进行中的授权。
    NoFlow,
    /// 进行中的那一次不收（state 不是它的，或没有 code 与 error）：它接着等。
    Refused,
    /// 收下了：等它的结局。
    Accepted(Outcome),
}

/// 等一次授权的结局：结束之前是 None。
pub(crate) type Outcome = watch::Receiver<Option<McpSignInOutcome>>;

/// 等到结局；授权任务没留下结局就没了（内核的作用域关了）时是 `Ended`。
pub(crate) async fn settled(mut outcome: Outcome) -> McpSignInOutcome {
    match outcome.wait_for(Option::is_some).await {
        Ok(done) => done.clone().unwrap_or(McpSignInOutcome::Ended),
        Err(_) => McpSignInOutcome::Ended,
    }
}

struct Flow {
    id: u64,
    target: McpAuthTarget,
    url: String,
    started_ms: u64,
    by_tool: bool,
    outcome: Outcome,
    cancel: CancellationToken,
    /// 用户贴回来的回调地址的入口（`finish` 在另一头收）。
    paste: mpsc::Sender<PastedCallback>,
}

/// 一次授权同时在等的贴回来的地址最多这么多个（模型一次只贴一个；满了就是没收下）。
const PASTE_QUEUE: usize = 1;

#[derive(Default)]
struct State {
    flows: Vec<Flow>,
    generations: Vec<(McpAuthTarget, u64)>,
    managers: Vec<Weak<Inner>>,
    next_flow: u64,
}

/// 授权任务的守卫：放下时从登记里拿掉这一次。
struct Forget {
    sign_ins: Arc<McpSignIns>,
    id: u64,
}

impl Drop for Forget {
    fn drop(&mut self) {
        let id = self.id;
        self.sign_ins.state().flows.retain(|flow| flow.id != id);
    }
}

/// 进程级的授权登记。
pub struct McpSignIns {
    authorizer: Arc<dyn McpAuthorizer>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl McpSignIns {
    pub fn new(authorizer: Arc<dyn McpAuthorizer>, clock: Arc<dyn Clock>) -> Self {
        Self {
            authorizer,
            clock,
            state: Mutex::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock_unpoisoned()
    }

    pub(crate) fn authorizer(&self) -> &Arc<dyn McpAuthorizer> {
        &self.authorizer
    }

    /// 管理器建好（内核级、会话级的兄弟）时登记；顺带清掉已经没了的。
    pub(crate) fn register(&self, manager: &Arc<Inner>) {
        let mut state = self.state();
        state.managers.retain(|known| known.strong_count() > 0);
        state.managers.push(Arc::downgrade(manager));
    }

    /// 这个目标的代号（没有变过是 0）。
    pub(crate) fn generation(&self, target: &McpAuthTarget) -> u64 {
        self.state()
            .generations
            .iter()
            .find(|(known, _)| known == target)
            .map_or(0, |(_, generation)| *generation)
    }

    /// 进行中的授权的地址。
    pub(crate) fn pending_url(&self, target: &McpAuthTarget) -> Option<String> {
        self.state()
            .flows
            .iter()
            .find(|flow| flow.target == *target)
            .map(|flow| flow.url.clone())
    }

    /// 按地址找进行中的那一次的结局；已经不在进行时 None（`Ended`）。
    pub(crate) fn outcome_of(&self, target: &McpAuthTarget, url: &str) -> Option<Outcome> {
        self.state()
            .flows
            .iter()
            .find(|flow| flow.target == *target && flow.url == url)
            .map(|flow| flow.outcome.clone())
    }

    /// 授权伪工具 180 s 内接着用自己开始的那一次（§3.12.16）。
    fn reusable(&self, target: &McpAuthTarget) -> Option<Started> {
        let now = self.clock.now_ms();
        let limit = u64::try_from(TOOL_REUSE.as_millis()).unwrap_or(u64::MAX);
        self.state()
            .flows
            .iter()
            .find(|flow| {
                flow.target == *target
                    && flow.by_tool
                    && now.saturating_sub(flow.started_ms) < limit
            })
            .map(|flow| Started {
                url: flow.url.clone(),
                reused: true,
                outcome: flow.outcome.clone(),
            })
    }

    /// 开始一次授权（§3.12.14）：`by_tool` 时先看能不能接着用；否则开始新的，丢下这个目标进行中的那一次。授权任务在 `spawner`
    /// （发起它的内核的作用域）里跑，不随发起的请求结束：等回调、换令牌、发布，成功之后重连这个进程里这个目标的要授权的连接。
    pub(crate) async fn begin(
        self: &Arc<Self>,
        target: McpAuthTarget,
        scope: Option<String>,
        by_tool: bool,
        spawner: &Spawner,
    ) -> Result<Started, String> {
        if by_tool && let Some(started) = self.reusable(&target) {
            return Ok(started);
        }
        let pending = self
            .authorizer
            .begin(&target, scope.as_deref())
            .await
            .map_err(|error| error.to_string())?;
        let url = pending.url().to_owned();
        let (settle, outcome) = watch::channel(None);
        let cancel = CancellationToken::new();
        let (paste, pasted) = mpsc::channel(PASTE_QUEUE);
        let id = {
            let mut state = self.state();
            if let Some(index) = state.flows.iter().position(|flow| flow.target == target) {
                state.flows.remove(index).cancel.cancel();
            }
            state.next_flow += 1;
            let id = state.next_flow;
            state.flows.push(Flow {
                id,
                target: target.clone(),
                url: url.clone(),
                started_ms: self.clock.now_ms(),
                by_tool,
                outcome: outcome.clone(),
                cancel: cancel.clone(),
                paste,
            });
            id
        };
        let sign_ins = self.clone();
        drop(spawner.spawn_resident("mcp-sign-in", async move {
            // 这一次结束（含任务随内核的作用域被丢下）就从登记里拿掉：不留下没人收的地址与贴回口
            let forget = Forget {
                sign_ins: sign_ins.clone(),
                id,
            };
            let ended = tokio::select! {
                () = cancel.cancelled() => McpSignInOutcome::Replaced,
                finished = pending.finish(pasted) => match finished {
                    Ok(()) => {
                        sign_ins.changed(&target, AuthChange::SignedIn).await;
                        McpSignInOutcome::SignedIn
                    }
                    Err(error) => McpSignInOutcome::Failed(error.to_string()),
                },
            };
            // 先拿掉再给结局：等结局的一方醒来时登记里已经没有它（servers 不再报它的地址）
            drop(forget);
            let _ = settle.send(Some(ended));
        }));
        Ok(Started {
            url,
            reused: false,
            outcome,
        })
    }

    /// 把用户贴回来的回调地址交给这个目标进行中的授权（§3.12.16），等它当场回答收不收。
    pub(crate) async fn paste(&self, target: &McpAuthTarget, url: &str) -> Paste {
        let flow = self
            .state()
            .flows
            .iter()
            .find(|flow| flow.target == *target)
            .map(|flow| (flow.paste.clone(), flow.outcome.clone()));
        let Some((paste, outcome)) = flow else {
            return Paste::NoFlow;
        };
        let (accepted, answer) = oneshot::channel();
        let callback = PastedCallback {
            url: url.to_owned(),
            accepted,
        };
        // 授权在这期间结束（口关了）：同没有进行中的
        if paste.send(callback).await.is_err() {
            return Paste::NoFlow;
        }
        match answer.await {
            Ok(true) => Paste::Accepted(outcome),
            Ok(false) => Paste::Refused,
            Err(_) => Paste::NoFlow,
        }
    }

    /// 删掉这个目标的令牌（§3.12.6），之后重连这个进程里这个目标的连接（§3.12.14）。
    pub(crate) async fn sign_out(&self, target: &McpAuthTarget) -> Result<(), String> {
        self.authorizer
            .sign_out(target)
            .await
            .map_err(|error| error.to_string())?;
        self.changed(target, AuthChange::SignedOut).await;
        Ok(())
    }

    /// 授权变了：代号加一，每个活着的管理器重连这个目标的相应连接，等它们都离开 Pending。
    async fn changed(&self, target: &McpAuthTarget, change: AuthChange) {
        let managers: Vec<Arc<Inner>> = {
            let mut state = self.state();
            match state
                .generations
                .iter_mut()
                .find(|(known, _)| known == target)
            {
                Some((_, generation)) => *generation += 1,
                None => state.generations.push((target.clone(), 1)),
            }
            state.managers.retain(|known| known.strong_count() > 0);
            state.managers.iter().filter_map(Weak::upgrade).collect()
        };
        let managers: Vec<McpManager> = managers.into_iter().map(McpManager).collect();
        join_all(
            managers
                .iter()
                .map(|manager| manager.auth_changed(target.clone(), change)),
        )
        .await;
    }
}

#[cfg(test)]
#[path = "sign_ins_tests.rs"]
mod tests;
