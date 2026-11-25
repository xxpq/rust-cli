//! 会话 actor 的一批提交（docs/specs/rust-cli/session-lifecycle.md §2）：当前轮由 actor 的 `RunLedger` 给出，续写时
//! 补在内存里、还没落库的消息排在第一批带消息的提交最前面（§6.3、§6.3.1）。先落库，调用方在成功之后才改内存、发事件。
//! 草稿（session-facts.md §4）：第一批带消息的提交之前不落库，会话行与攒下的事实排在那一批最前面。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{CommandId, MessageId, TurnId};
use rust_cli_kernel_api::session_api::{Ack, KernelEvent, TurnOrigin, TurnStart};
use rust_cli_kernel_api::store::{Fact, FactBatch, StoreError, TurnAnchor, TurnRecord};

use crate::actor::{Actor, accepted, rejected};

impl Actor {
    /// 提交一批（先落库，再改内存、发事件：调用方在成功之后才改状态）。当前轮由 actor 的 `RunLedger` 给出（性能修复，
    /// 修复线 F6，审查 R5-3：存储不再为 assistant 与每个用量行各从库里推导一次），提交成功后才记下。
    ///
    /// 失败原样给出存储的错误（工作线 F130，审查 C15）：以前转成字符串，`NotFound` 与别的失败分不开，
    /// 回合失败时再包一层 `StoreError::Storage` 交给主循环（文字成了 `storage error: storage error: …`）。
    pub(crate) async fn commit(
        &mut self,
        command: Option<CommandId>,
        facts: Vec<Fact>,
    ) -> Result<(), StoreError> {
        if self.write_blocked.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable {
                message: "session storage is paused; reopen to reconcile".into(),
            });
        }
        let carries_messages = facts
            .iter()
            .any(|fact| matches!(fact, Fact::MessageAppended { .. }));
        // 草稿（02 §11 M2）：不带消息的批次（换模型、换模式、改名……）不落库，照常由调用方改内存、发事件；第一批带消息的
        // 连草稿一起落库。提交失败时草稿不动，下一批再试。
        let held = match &mut self.draft {
            Some(draft) if !carries_messages => {
                draft.extend(facts);
                return Ok(());
            }
            Some(draft) => draft.clone(),
            None => Vec::new(),
        };
        // 开始了、还没有消息落库的回合：它的记录与这一批里它的第一条消息同一批（session-lifecycle.md §10.1）。草稿与续写时
        // 补在前面的消息不算这个回合的，所以在它们排进来之前取。
        let record = self.unrecorded.clone().and_then(|opened| {
            facts.iter().find_map(|fact| match fact {
                Fact::MessageAppended { message } => Some(TurnRecord {
                    turn: opened.turn.clone(),
                    first: message.id().clone(),
                    origin: opened.origin,
                    started_at: opened.started_at,
                    command: opened.command.clone(),
                    ended: None,
                    fell_back: None,
                    works: opened.works.clone(),
                    compactions: Vec::new(),
                }),
                _ => None,
            })
        });
        let pending = self.pending_resume(&facts);
        let stored_pending = !pending.is_empty();
        let facts = held
            .into_iter()
            .chain(pending)
            .chain(facts)
            .chain(record.iter().map(TurnRecord::fact))
            .collect();
        // 开回合的输入锚在在跑的回合上（K-I1）：没有回合在跑时（空闲时提交的本地命令记录）才新生成。
        let running = self.running.as_ref().map(|running| running.turn.clone());
        let ids = self.deps.ids.clone();
        let batch = self
            .ledger
            .batch(command, facts, || running.unwrap_or_else(|| ids.turn()));
        let turn = batch.turn.clone();
        self.deps
            .store
            .commit(&self.meta.id, batch)
            .await
            .map_err(|error| self.pause_writes(error))?;
        self.ledger.committed(turn);
        self.draft = None;
        if stored_pending {
            self.resume_pending.clear();
        }
        if let Some(record) = record {
            self.unrecorded = None;
            self.turns.push(record);
        }
        Ok(())
    }

    /// 回合开始：这个回合的记录等它的第一条消息（session-lifecycle.md §10.1）；来源、开始时刻（§10.8）与开它的后台工作（§10.50）记在记录里，
    /// `TurnStarted` 带同样的值。返回要发的 `TurnStarted`：通知开的回合先提交消息再发它（turn.rs）。
    pub(crate) fn open_record(
        &mut self,
        turn: TurnId,
        command: Option<CommandId>,
        origin: TurnOrigin,
        works: Vec<rust_cli_kernel_api::background::NoticedWork>,
    ) -> KernelEvent {
        let started_at = self.deps.clock.now_ms();
        self.unrecorded = Some(TurnStart {
            turn: turn.clone(),
            command: command.clone(),
            origin,
            started_at,
            works: works.clone(),
        });
        KernelEvent::TurnStarted {
            turn,
            command,
            origin,
            started_at,
            works,
        }
    }

    /// 在跑回合的开始（视图的 `running`，session-lifecycle.md §10.33、§10.35）：第一条消息落库之前在 `unrecorded`，之后在它的回合记录里。
    /// 只按阶段换位置，不另存一份；在且只在有回合在跑时有值（开回合的入口都先 `open_record` 再 `launch`）。
    pub(crate) fn running_start(&self) -> Option<TurnStart> {
        let running = self.running.as_ref()?;
        self.unrecorded.clone().or_else(|| {
            self.turns
                .iter()
                .rev()
                .find(|record| record.turn == running.turn)
                .map(TurnRecord::start)
        })
    }

    /// 回合开始：发 `TurnStarted`（[`Self::open_record`]）。人类输入、bash 模式与压缩的回合经这里。
    pub(crate) fn turn_started(
        &mut self,
        turn: TurnId,
        command: Option<CommandId>,
        origin: TurnOrigin,
    ) {
        let started = self.open_record(turn.clone(), command, origin, Vec::new());
        self.log.append(started);
        self.attach_session_hooks(&turn, origin);
    }

    /// 续写时补的消息还没落库、这一批有消息：返回它们，按历史里的顺序排在这一批最前面（工作线 F77、F81，
    /// session-lifecycle.md §6.3、§6.3.1）。已不在活动历史里（被回退掉）的丢掉。
    fn pending_resume(&mut self, facts: &[Fact]) -> Vec<Fact> {
        if !facts
            .iter()
            .any(|fact| matches!(fact, Fact::MessageAppended { .. }))
        {
            return Vec::new();
        }
        let found = self.resume_facts(&self.resume_pending);
        if found.is_empty() {
            self.resume_pending.clear();
        }
        found
    }

    /// 续写时补的消息里还在活动历史里的那些（被回退掉的丢掉）。
    fn resume_facts(&self, pending: &[MessageId]) -> Vec<Fact> {
        pending
            .iter()
            .filter_map(|id| self.messages.iter().find(|message| message.id() == id))
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect()
    }

    /// 会话里切到这个会话（`/resume`、`/fork`）之后：续写时补在内存里、还没落库的消息（末尾未结调用的中断结果与
    /// `No response requested.`）马上单独提交一批（工作线 F79、F81，session-lifecycle.md §6.3、§6.3.1）。没有要写的时候
    /// 什么都不做；提交失败时放回去，随下一批带消息的提交写。
    pub(crate) async fn store_resume_sentinel(&mut self, id: CommandId) -> Ack {
        // 先取下再提交：`commit` 看到这一批有消息时不会再把它们排进来一次。
        let pending = std::mem::take(&mut self.resume_pending);
        let facts = self.resume_facts(&pending);
        if facts.is_empty() {
            return accepted();
        }
        match self.commit(Some(id), facts).await {
            Ok(()) => accepted(),
            Err(reason) => {
                self.resume_pending = pending;
                rejected(Refusal::Failed, reason)
            }
        }
    }

    /// 提交一批锚在给定回合的事实，不改当前轮：起任务时记下回合、结果晚到的旁路请求用（会话标题的用量行，
    /// usage.md §2.1.1，工作线 F73）。
    pub(crate) async fn commit_in_turn(
        &self,
        turn: Option<TurnAnchor>,
        facts: Vec<Fact>,
    ) -> Result<(), StoreError> {
        let batch = FactBatch {
            command: None,
            facts,
            turn,
        };
        self.deps
            .store
            .commit(&self.meta.id, batch)
            .await
            .map(|_| ())
            .map_err(|error| self.pause_writes(error))
    }

    pub(crate) fn turn_anchor(&self) -> Option<TurnAnchor> {
        self.ledger.turn().cloned()
    }
}
