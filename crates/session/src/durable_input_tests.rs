use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use rust_cli_kernel_api::ids::{CommandId, SessionId};
use rust_cli_kernel_api::input_journal::*;
use rust_cli_kernel_api::session_api::{Ack, Command, CommandEnvelope, Delivery, SessionHandle};
use rust_cli_kernel_api::session_policy::{BashInputCommit, ResumeStart, SessionPolicy};
use rust_cli_kernel_api::store::*;
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, text_response};

use crate::test_support::{Setup, meta};

#[derive(Default)]
struct DurableStore {
    inner: Arc<MemoryStore>,
    fail_admission: AtomicBool,
    unknown_admission: AtomicBool,
    fail_commit: AtomicBool,
    inputs: Mutex<Vec<JournalInput>>,
}

#[async_trait]
impl SessionStore for DurableStore {
    async fn create(
        &self,
        meta: SessionMeta,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        self.inner.create(meta, batch).await
    }
    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        if self.fail_commit.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable {
                message: "test database connection lost".into(),
            });
        }
        // 同 pg-store 的 `promote_inputs`：要提升的命令不在账本里（或不是待处理）时整批失败。
        for fact in &batch.facts {
            if let Fact::InputsPromoted { commands, .. } = fact {
                let inputs = self.inputs.lock().unwrap();
                if let Some(unknown) = commands.iter().find(|command| {
                    !inputs.iter().any(|input| {
                        input.envelope.id == **command && input.state == InputState::Pending
                    })
                }) {
                    return Err(StoreError::Conflict {
                        command_id: unknown.to_string(),
                    });
                }
            }
        }
        let receipt = self.inner.commit(session, batch.clone()).await?;
        let mut inputs = self.inputs.lock().unwrap();
        for fact in batch.facts {
            if let Fact::InputsPromoted { commands, message } = fact {
                for input in &mut *inputs {
                    if commands.contains(&input.envelope.id) {
                        input.state = InputState::Promoted;
                        input.promoted_message = Some(message.clone());
                    }
                }
            }
        }
        Ok(receipt)
    }
    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        self.inner.load(session).await
    }
    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        self.inner.list(filter).await
    }
    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, serde_json::Value)>, StoreError> {
        self.inner.entries(session, kind).await
    }
    async fn fork(&self, fork: SessionFork) -> Result<ForkReceipt, StoreError> {
        self.inner.fork(fork).await
    }
    async fn side_chat(&self, fork: SideChatFork) -> Result<ForkReceipt, StoreError> {
        self.inner.side_chat(fork).await
    }
    async fn query_sessions(&self, query: SessionQuery) -> Result<SessionPage, StoreError> {
        self.inner.query_sessions(query).await
    }
    async fn rename(&self, session: &SessionId, title: &str) -> Result<(), StoreError> {
        self.inner.rename(session, title).await
    }
    async fn delete(&self, session: &SessionId) -> Result<(), StoreError> {
        self.inner.delete(session).await
    }
}

#[async_trait]
impl InputJournal for DurableStore {
    async fn admit(
        &self,
        _session: &SessionId,
        envelope: &CommandEnvelope,
    ) -> Result<InputAdmission, StoreError> {
        if self.fail_admission.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable {
                message: "test database unavailable".into(),
            });
        }
        if self.unknown_admission.load(Ordering::SeqCst) {
            return Err(StoreError::OutcomeUnknown);
        }
        let mut inputs = self.inputs.lock().unwrap();
        if let Some(old) = inputs.iter().find(|input| input.envelope.id == envelope.id) {
            if old.envelope != *envelope {
                return Err(StoreError::Conflict {
                    command_id: envelope.id.to_string(),
                });
            }
            return Ok(InputAdmission {
                input: old.clone(),
                duplicate: true,
            });
        }
        let input = JournalInput {
            envelope: envelope.clone(),
            sequence: inputs.len() as u64 + 1,
            state: InputState::Pending,
            receipt: Ack::Accepted {
                result: rust_cli_kernel_api::session_api::CommandResult::Done,
            },
            promoted_message: None,
        };
        inputs.push(input.clone());
        Ok(InputAdmission {
            input,
            duplicate: false,
        })
    }
    async fn pending(&self, _session: &SessionId) -> Result<Vec<JournalInput>, StoreError> {
        Ok(self
            .inputs
            .lock()
            .unwrap()
            .iter()
            .filter(|input| input.state == InputState::Pending)
            .cloned()
            .collect())
    }
    async fn input(
        &self,
        _session: &SessionId,
        command: &CommandId,
    ) -> Result<Option<JournalInput>, StoreError> {
        Ok(self
            .inputs
            .lock()
            .unwrap()
            .iter()
            .find(|input| input.envelope.id == *command)
            .cloned())
    }
    async fn cancel(&self, _session: &SessionId, commands: &[CommandId]) -> Result<(), StoreError> {
        for input in &mut *self.inputs.lock().unwrap() {
            if commands.contains(&input.envelope.id) {
                input.state = InputState::Cancelled;
            }
        }
        Ok(())
    }
    fn promotion(
        &self,
        commands: Vec<CommandId>,
        message: &rust_cli_kernel_api::ids::MessageId,
    ) -> Vec<Fact> {
        promoted(commands, message)
    }
}

/// 会话库与输入账本是同一个替身；策略是有持久账本的部署的那一组（恢复时等用户的下一条输入、bash 输入先提交）。
fn durable(setup: &mut Setup, store: &Arc<DurableStore>) {
    setup.store_port = Some(store.clone());
    setup.journal = Some(store.clone());
    setup.policy = SessionPolicy {
        resume: ResumeStart::OnNextInput,
        bash_input: BashInputCommit::BeforeRun,
        ..SessionPolicy::default()
    };
}

fn submit(id: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Submit {
            text: text.into(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        },
    }
}

#[tokio::test]
async fn failed_durable_admission_never_returns_accepted() {
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    store.fail_admission.store(true, Ordering::SeqCst);
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["answer"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    let built = setup.build().await;
    let ack = built.session.send(submit("input", "hello")).await;
    assert!(
        matches!(ack, Ack::Rejected { .. }),
        "must fail before ACK: {ack:?}"
    );
    assert!(store.inputs.lock().unwrap().is_empty());
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn cold_pending_inputs_are_visible_without_automatically_running() {
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let input = store
        .admit(&meta().id, &submit("pending", "keep me"))
        .await
        .unwrap()
        .input;
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["answer"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    setup.opened = crate::Opened::Loaded;
    setup.pending_inputs = vec![input];
    let built = setup.build().await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue.len(), 1);
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn a_hook_block_cancels_the_durable_input() {
    use crate::test_support::{ScriptedHooks, finished, until};
    use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let model = Arc::new(ScriptedModel::new(vec![]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    setup.hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            blocking_error: Some("blocked".into()),
            ..HookOutcome::default()
        },
    ));
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    assert!(matches!(
        built.session.send(submit("blocked", "no")).await,
        Ack::Accepted { .. }
    ));
    until(&mut events, finished).await;
    assert!(store.pending(&meta().id).await.unwrap().is_empty());
    assert_eq!(
        store
            .input(&meta().id, &CommandId::new("blocked"))
            .await
            .unwrap()
            .unwrap()
            .state,
        InputState::Cancelled
    );
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn unconfirmed_admission_is_unknown_not_rejected() {
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    store.unknown_admission.store(true, Ordering::SeqCst);
    let model = Arc::new(ScriptedModel::new(vec![]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    let built = setup.build().await;
    assert!(matches!(
        built.session.send(submit("unknown", "keep the ID")).await,
        Ack::OutcomeUnknown { .. }
    ));
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn a_resume_that_waits_for_input_defers_start_hooks_until_the_user_continues() {
    use crate::test_support::{ScriptedHooks, finished, until};
    use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
    use rust_cli_kernel_api::session_api::StartSource;
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    setup.opened = crate::Opened::Loaded;
    setup.start = StartSource::Resume;
    setup.hooks = Arc::new(ScriptedHooks::with(
        HookEventName::SessionStart,
        HookOutcome {
            session_title: Some("hook ran".into()),
            ..HookOutcome::default()
        },
    ));
    let built = setup.build().await;
    assert_ne!(built.session.view().await.unwrap().meta.title, "hook ran");
    assert!(model.requests().is_empty());
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("continue", "continue")).await;
    until(&mut events, finished).await;
    assert_eq!(built.session.view().await.unwrap().meta.title, "hook ran");
}

// CR-01 回归：输入已接纳后提交断线（Unavailable），会话暂停；后续输入不能越过 pending 的旧输入执行。
#[tokio::test]
async fn lost_connection_after_admission_pauses_later_inputs() {
    use crate::test_support::{finished, until};
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    store.fail_commit.store(true, Ordering::SeqCst);
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["answer"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    assert!(matches!(
        built.session.send(submit("first", "one")).await,
        Ack::Accepted { .. }
    ));
    until(&mut events, finished).await;
    store.fail_commit.store(false, Ordering::SeqCst);
    assert!(matches!(
        built.session.send(submit("second", "two")).await,
        Ack::Rejected { .. }
    ));
    let pending = store.pending(&meta().id).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].envelope.id, CommandId::new("first"));
    assert!(model.requests().is_empty());
}

// 回归（审查 2026-11-11）：用户取回一条排队输入后，账本里它仍是待处理，重开会话后回到队列并执行。
#[tokio::test]
async fn a_dequeued_input_is_cancelled_in_the_journal() {
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let input = store
        .admit(&meta().id, &submit("pending", "keep me"))
        .await
        .unwrap()
        .input;
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["answer"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    setup.opened = crate::Opened::Loaded;
    setup.pending_inputs = vec![input];
    let built = setup.build().await;
    assert_eq!(built.session.view().await.unwrap().queue.len(), 1);
    let dequeue = |id: &str| CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Dequeue {
            command: CommandId::new("pending"),
        },
    };
    let ack = built.session.send(dequeue("take-back")).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "dequeue ack: {ack:?}");
    assert_eq!(built.session.view().await.unwrap().queue.len(), 0);
    assert_eq!(
        store
            .input(&meta().id, &CommandId::new("pending"))
            .await
            .unwrap()
            .unwrap()
            .state,
        InputState::Cancelled
    );
    assert!(store.pending(&meta().id).await.unwrap().is_empty());
    // 已经不在队列里：再取回一次是“已发出”。
    let again = built.session.send(dequeue("take-back-again")).await;
    assert!(
        matches!(again, Ack::Rejected { .. }),
        "second dequeue: {again:?}"
    );
    assert!(model.requests().is_empty());
}

// 回归（MERGE-CLOUD）：别的进程接纳过的命令，这里回它原来的回执；在本进程里再重发一次，回的仍是那个原回执，不是一层套一层的
// `Duplicate`（会话的命令账本记下包着的原结果，ledger.rs 的 `settle`）。
#[tokio::test]
async fn a_command_another_process_admitted_repeats_its_original_receipt() {
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    store
        .admit(&meta().id, &submit("one", "hello"))
        .await
        .unwrap();
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["answer"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    let built = setup.build().await;
    let original = Box::new(Ack::Accepted {
        result: rust_cli_kernel_api::session_api::CommandResult::Done,
    });
    for attempt in 0..2 {
        let ack = built.session.send(submit("one", "hello")).await;
        assert_eq!(
            ack,
            Ack::Duplicate {
                original: original.clone()
            },
            "attempt {attempt}"
        );
    }
    assert!(model.requests().is_empty());
}

// 回归（MERGE-CLOUD）：v5 的 `SubmitFollowUp` 不进账本（`is_journal_input` 不认它）。以前按命令 id 的前缀认出不在账本里的输入，
// v5 的 id 没有前缀，这条输入的消息落库时带上它的提升，持久账本报冲突、回合失败。现在只提升记过账的命令（durable_inputs.rs
// 的 `Journaled`）。
#[tokio::test]
async fn an_input_the_journal_never_saw_is_not_promoted() {
    let store = Arc::new(DurableStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["answer"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store.inner.clone());
    durable(&mut setup, &store);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let follow_up = CommandEnvelope {
        id: CommandId::new("follow-up"),
        command: Command::SubmitFollowUp {
            text: "hello".into(),
            extras: Default::default(),
        },
    };
    let ack = built.session.send(follow_up).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let seen = crate::test_support::until(&mut events, crate::test_support::finished).await;
    let finished = seen.last().unwrap();
    assert!(
        matches!(
            finished,
            rust_cli_kernel_api::session_api::KernelEvent::TurnFinished {
                outcome: rust_cli_kernel_api::session_api::TurnOutcome::Success,
                ..
            }
        ),
        "{finished:?}"
    );
    assert!(store.inputs.lock().unwrap().is_empty());
    assert_eq!(model.requests().len(), 1);
}
