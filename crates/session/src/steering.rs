//! 插话在工具边界并入前的 UserPromptSubmit（docs/specs/rust-cli/query.md §4.1），与同一工具边界的后台通知
//! （§5，L18）。
//!
//! 队列与 next 通道只由 actor 持有：主循环经 [`ActorSink`] 先领取（不出队），在主循环的任务里跑 hook
//! （不挡 actor），再把每条插话的结局交回 actor 出队或留下。通知不跑 hook，交回时 actor 给并入的每条通知一个
//! attachment 的消息 ID，那条消息提交之后才出通道（lane.rs）。领取与结局之间队列可能被改动，actor 只按命令 ID 处理还在
//! 队列里的插话、按通知的身份（只在内存的 `NoticeKey`）处理通知；领取之后被取回（`Command::Dequeue`，query.md §4.4）
//! 的插话不再在队列里，主循环只并入 actor 回复说出了队的插话。

use std::sync::Arc;

use rust_cli_kernel_api::hook_runs;
use rust_cli_kernel_api::hooks::{HookEvent, HookEventName, HookOutcome, HookRunner};
use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::orchestration::{Absorbed, AbsorbedNotice, Steered};
use rust_cli_kernel_api::session_api::{DropReason, InputFate, Joined, KernelEvent, NoticeLevel};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::actor::Actor;
use crate::inbox::{LaneItem, NoticeKey, Screening, is_slash};
use crate::sink::{ActorSink, SinkRequest};

/// 领取到的一项，按通道顺序。
pub(crate) enum ClaimedItem {
    /// 插话，带组装好的 UPS 事件。
    Prompt(CommandId, String, Box<HookEvent>),
    /// 通知与它的全文。
    Notice(NoticeKey, String),
}

/// 领取到的插话与通知；没有 UPS hook 时 `hooks` 为 None。
pub(crate) struct Claimed {
    pub(crate) items: Vec<ClaimedItem>,
    pub(crate) hooks: Option<Arc<dyn HookRunner>>,
    pub(crate) abort: CancellationToken,
}

/// 交回结局后 actor 的回复：出了队、可以并入的插话（领取之后被取回的不在其中）与它们的 attachment 消息 ID，与并入的
/// 每条通知（按顺序）的 attachment 消息 ID。
#[derive(Default)]
pub(crate) struct Settled {
    pub(crate) absorbed: Vec<(CommandId, MessageId)>,
    pub(crate) notices: Vec<MessageId>,
}

/// 一条插话的结局。
pub(crate) enum Fate {
    /// 并入本回合：出队。
    Absorbed,
    /// UPS 阻塞：出队，不并入；带给界面的提示（query.md §4.2）。
    Dropped(Option<String>),
    /// 留到回合结束后作为独立回合；有结果时那个回合直接用它，不再跑 hook。
    Waits(Option<Box<HookOutcome>>),
}

impl Actor {
    /// 按通道顺序给出插话与通知。遇到留下的插话后不再给插话（筛查屏障只挡 `prompt`），通知照常
    /// 给出。已中止或没有回合时不领取，都继续排队。
    pub(crate) fn claim_steering(&self) -> Option<Claimed> {
        let running = self.running.as_ref()?;
        if running.abort.is_aborted() {
            return None;
        }
        let mut items = Vec::new();
        let mut barrier = false;
        for item in self.inbox.lane() {
            match item {
                LaneItem::Input(queued) => {
                    if barrier || matches!(queued.screening, Screening::Held(_)) {
                        barrier = true;
                        continue;
                    }
                    let input = &queued.input;
                    // 斜杠命令不在工具边界并入，留在通道里，回合结束后作为独立回合展开（query.md §4，L22）；它不是屏障，
                    // 后面的插话照常领取。
                    if is_slash(&input.text) {
                        continue;
                    }
                    let event = self.user_prompt_submit(&input.text);
                    items.push(ClaimedItem::Prompt(
                        input.command.clone(),
                        input.text.clone(),
                        Box::new(event),
                    ));
                }
                LaneItem::Notice(notice) => {
                    items.push(ClaimedItem::Notice(notice.key, notice.waiting.text()));
                }
            }
        }
        let hooks = self
            .deps
            .hooks
            .has_hooks(HookEventName::UserPromptSubmit)
            .then(|| self.deps.hooks.clone());
        Some(Claimed {
            items,
            hooks,
            abort: running.abort.token().clone(),
        })
    }

    /// 结局：并入与丢弃的出队（丢弃时发提示）；留下的记下结果（回合结束后由 `start_next` 取用）。并入的通知（按顺序）
    /// 各得到一个 attachment 的消息 ID，返回给主循环；提交之后按它出通道（C8）。同时返回出了队、可以并入的插话。
    ///
    /// 并入的插话在这一步里套用它的执行配置（session-lifecycle.md §10.25，主会话审定 2026-11-06）：模式与 plan 马上是会话的，这一轮之后的
    /// 判决按它；模型写成会话的选择，这一次运行不换，下一轮用它。按通道顺序套用，后一条的胜。
    pub(crate) async fn settle_steering(
        &mut self,
        fates: Vec<(CommandId, Fate)>,
        notices: Vec<NoticeKey>,
    ) -> Settled {
        let mut absorbed = Vec::new();
        // 领取之后被取回（`Dequeue`）的插话不在队列里了：每一种结局都只处理还在的。
        for (command, fate) in fates {
            match fate {
                Fate::Absorbed => {
                    let Some(queued) = self.inbox.remove_input(&command) else {
                        continue;
                    };
                    // 套用落库失败：同回合里别的写入失败，中止这一轮、之后的提交一律拒绝（这条插话也就进不了对话）
                    if let Err(error) = self.adopt(&queued.input.extras.config).await {
                        self.fail(error);
                    }
                    // attachment 的消息 ID 由 actor 给：提交它的批次带这条插话的命令 ID（效果记录，session-lifecycle.md
                    // §10.3）。以前主循环自己取，并入的插话落库时没有命令 ID。
                    let id = self.deps.ids.message();
                    if let Some(running) = &mut self.running {
                        running.inputs.push((id.clone(), command.clone()));
                        let turn = running.turn.clone();
                        self.input_fate(
                            command.clone(),
                            InputFate::Started {
                                turn,
                                joined: Joined::Absorbed,
                            },
                        );
                    }
                    absorbed.push((command, id));
                }
                Fate::Dropped(notice) => {
                    // 先在账本里记取消，再从内存队列去掉（`discard_input`）；失败时这条留在队列里（账本里仍是待处理），
                    // 其余结局照常处理，并入通知的消息 ID 仍按顺序给出。领取之后被取回的不在队列里了，跳过。
                    match self.discard_input(&command).await {
                        Ok(Some(_)) => {}
                        Ok(None) => continue,
                        Err(error) => {
                            let reason = self.store_failed(error);
                            tracing::warn!(%reason, "durable steering cancellation failed");
                            continue;
                        }
                    }
                    self.input_fate(
                        command.clone(),
                        InputFate::Dropped {
                            why: DropReason::Blocked,
                        },
                    );
                    // 只供显示，排在本次工具边界的 queued_command 之前（先给出 notices）。正文是 hook 的阻塞说明拼成的一段，没有种类，
                    // 前端显示原文（query.md §4.2）。
                    if let Some(text) = notice {
                        self.log.append(KernelEvent::SystemNotice {
                            level: NoticeLevel::Warning,
                            text,
                            kind: None,
                        });
                    }
                }
                Fate::Waits(outcome) => self.inbox.hold(&command, outcome),
            }
        }
        self.emit_queue();
        let notices = notices
            .into_iter()
            .map(|key| {
                let id = self.deps.ids.message();
                self.inbox.absorbing(key, id.clone());
                id
            })
            .collect();
        Settled { absorbed, notices }
    }
}

/// 领取之后、交回之前的一项。
enum Screened {
    Prompt(CommandId, String, Box<HookOutcome>),
    Notice(NoticeKey, String),
}

/// 交回之前的并入项：插话要等 actor 说出了队才并入，通知的消息 ID 要等 actor 给出。
enum Taken {
    Prompt(CommandId, Box<HookOutcome>, String),
    Notice(String),
}

impl ActorSink {
    async fn claim(&self) -> Option<Claimed> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::ClaimSteering { reply })
            .await
            .ok()?;
        answer.await.ok().flatten()
    }

    /// 领取 → 逐条跑插话的 UPS → 交回结局（query.md §4.1 的表），按到达顺序给出插话与通知。
    pub(crate) async fn absorbed(&self) -> Vec<Absorbed> {
        let Some(claimed) = self.claim().await else {
            return Vec::new();
        };
        let mut fates = Vec::new();
        let mut screened = Vec::new();
        let mut barrier = false;
        for item in claimed.items {
            let (command, text, event) = match item {
                ClaimedItem::Notice(key, text) => {
                    screened.push(Screened::Notice(key, text));
                    continue;
                }
                ClaimedItem::Prompt(..) if barrier => continue,
                ClaimedItem::Prompt(command, text, event) => (command, text, event),
            };
            let outcome = match &claimed.hooks {
                Some(hooks) => hooks.run(*event, claimed.abort.clone()).await,
                None => HookOutcome::default(),
            };
            // hook 行的事实（§10.51）：挂在在跑的回合上
            hook_runs::report(self, HookEventName::UserPromptSubmit, None, &outcome).await;
            if claimed.abort.is_cancelled() {
                // 中止：这条没有可信的结果，回合结束后重跑 hook。
                fates.push((command, Fate::Waits(None)));
                break;
            }
            if outcome.blocking_error.is_some() {
                fates.push((command, Fate::Dropped(outcome.notices.first().cloned())));
                continue;
            }
            if outcome.prevent_continuation {
                // 留下，之后的插话本次也不并入（屏障）；通知不受影响。
                fates.push((command, Fate::Waits(Some(Box::new(outcome)))));
                barrier = true;
                continue;
            }
            screened.push(Screened::Prompt(command, text, Box::new(outcome)));
        }
        // 领取后回合被中止：全部留下（"leaving commands queued"），已跑完的记下结果；通知不并入，
        // 留在通道里。
        let aborted = claimed.abort.is_cancelled();
        let mut taken = Vec::new();
        let mut notices = Vec::new();
        for item in screened {
            match item {
                Screened::Prompt(command, _, outcome) if aborted => {
                    fates.push((command, Fate::Waits(Some(outcome))));
                }
                Screened::Notice(..) if aborted => {}
                Screened::Prompt(command, text, outcome) => {
                    fates.push((command.clone(), Fate::Absorbed));
                    taken.push(Taken::Prompt(command, outcome, text));
                }
                Screened::Notice(key, text) => {
                    notices.push(key);
                    taken.push(Taken::Notice(text));
                }
            }
        }
        let (reply, answer) = oneshot::channel();
        let settled = match self
            .sender
            .send(SinkRequest::SettleSteering {
                fates,
                notices,
                reply,
            })
            .await
        {
            Ok(()) => answer.await.unwrap_or_default(),
            Err(_) => Settled::default(),
        };
        // 修复：以前领取时就定下要并入的插话，不看结局交回时它还在不在队列里。领取与交回之间 actor 照常处理命令
        // （UPS hook 跑得越久窗口越大），这时取回（`Command::Dequeue`）的插话已回到用户的输入框，却仍被并入本回合：
        // 同一条消息既取回了又发了出去。现在只并入 actor 回复说出了队的插话：队列只由 actor 持有，出队与取回谁先到谁算。
        // 回合已经结束（通道关了）时拿不到回复：插话都不并入（留在队列里），通知的 ID 主循环自己取，提交时不再有人认它
        // （与回合一起结束）。
        let mut ids = settled.notices.into_iter();
        let mut absorbed = Vec::new();
        for item in taken {
            match item {
                Taken::Prompt(command, outcome, text) => {
                    let Some(id) = settled
                        .absorbed
                        .iter()
                        .find_map(|(absorbed, id)| (*absorbed == command).then(|| id.clone()))
                    else {
                        continue;
                    };
                    if let Some(title) = outcome.session_title.clone() {
                        self.title(title).await;
                    }
                    absorbed.push(Absorbed::Prompt(Steered {
                        command: Some(command),
                        text,
                        after: outcome.messages,
                        id: Some(id),
                    }));
                }
                Taken::Notice(text) => absorbed.push(Absorbed::Notice(AbsorbedNotice {
                    text,
                    id: ids.next(),
                })),
            }
        }
        absorbed
    }
}
