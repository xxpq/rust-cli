//! 效果记录（session-lifecycle.md §10.3，K-A2）：每条落进对话的输入，它的消息带自己的命令 id；冷读把这些 id 交回账本。

use super::*;
use crate::test_support::{Setup, until};

fn steer(id: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Submit {
            text: text.to_owned(),
            extras: Default::default(),
            delivery: Delivery::Steer,
            target: None,
        },
    }
}

/// 冷读的效果记录里的命令，按写入顺序、不重复（命令账本收回的就是它们，session-lifecycle.md §10.33）。
async fn effects(store: &MemoryStore) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    for source in store.load(&SessionId::new("sess_1")).await.unwrap().sources {
        if !commands
            .iter()
            .any(|known| known == source.command.as_str())
        {
            commands.push(source.command.as_str().to_owned());
        }
    }
    commands
}

/// 以前只有开回合的那条输入带命令 id；工具边界并入的插话（`queued_command` attachment）没有。
#[tokio::test]
async fn a_steered_input_records_its_command_id() {
    let scope = TaskScope::root("test", |_| {});
    let (store, _model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(steer("c2", "also b")).await;
    session.send(answer("c3", interaction)).await;
    until_finished(&mut events, 1).await;
    assert_eq!(effects(&store).await, ["c1", "c2"]);
}

/// 以前成批并进一个回合的后续输入（query.md §4.3）不带命令 id。
#[tokio::test]
async fn batched_inputs_record_their_command_ids() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["ok"]), text_response(&["both"])])
            .gated_at(0, gate.clone()),
    );
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(submit("c2", "one")).await;
    built.session.send(submit("c3", "two")).await;
    gate.notify_one();
    until_finished(&mut events, 2).await;
    assert_eq!(effects(&built.store).await, ["c1", "c2", "c3"]);
}

/// 以前新打开的 actor 账本是空的：进程重启之后重发同一条输入会再执行一次。账本读冷读的 `sources`（session-lifecycle.md
/// §10.33）：以前读另一份去重后的 id 集合 `LoadedSession.commands`，只给 `sources` 时账本是空的。
#[tokio::test]
async fn a_reopened_session_knows_the_stored_commands() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(model.clone());
    setup.opened = crate::Opened::Loaded;
    setup.sources = vec![rust_cli_kernel_api::store::InputSource {
        message: rust_cli_kernel_api::ids::MessageId::new("msg_1"),
        command: CommandId::new("c1"),
    }];
    let built = setup.build().await;
    assert_eq!(
        built.session.send(submit("c1", "hello")).await,
        Ack::Duplicate {
            original: Box::new(Ack::Accepted {
                result: rust_cli_kernel_api::session_api::CommandResult::Done
            })
        }
    );
    assert!(model.requests().is_empty());
    assert!(built.session.view().await.unwrap().running.is_none());
}

/// 每个 `InputCommitted` 的命令，并核对它紧排在同一条消息的 `MessageDone` 之前（session-lifecycle.md §10.31）。
fn committed(events: &[KernelEvent]) -> Vec<String> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match event {
            KernelEvent::InputCommitted { source } => {
                assert!(
                    matches!(&events[at + 1], KernelEvent::MessageDone { message } if *message.id() == source.message),
                    "the source comes right before its message"
                );
                Some(source.command.as_str().to_owned())
            }
            _ => None,
        })
        .collect()
}

/// 输入的消息与它的命令（§10.30）：开回合的输入与工具边界并入的插话各发一次 `InputCommitted`；视图与库里读回的同值。
/// 以前这条关联只在存储里，前端给不出「这条命令的消息出现了」。
#[tokio::test]
async fn a_steered_input_is_committed_with_its_command() {
    let scope = TaskScope::root("test", |_| {});
    let (store, _model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    let mut all = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(steer("c2", "also b")).await;
    session.send(answer("c3", interaction)).await;
    until_finished(&mut events, 1).await;
    assert_eq!(committed(&until_finished(&mut all, 1).await), ["c1", "c2"]);
    let view = session.view().await.unwrap();
    let loaded = store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(view.sources, loaded.sources);
    assert_eq!(view.sources.len(), 2);
}

/// 成批进一个回合的输入（query.md §4.3）各是自己的消息、各带自己的命令：一条消息只对应一条命令。
#[tokio::test]
async fn batched_inputs_are_committed_with_their_own_commands() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["ok"]), text_response(&["both"])])
            .gated_at(0, gate.clone()),
    );
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(submit("c2", "one")).await;
    built.session.send(submit("c3", "two")).await;
    gate.notify_one();
    until_finished(&mut events, 2).await;
    let mut all = built.session.subscribe(Some(0));
    assert_eq!(
        committed(&until_finished(&mut all, 2).await),
        ["c1", "c2", "c3"]
    );
    let view = built.session.view().await.unwrap();
    let loaded = built.store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(view.sources, loaded.sources);
}
