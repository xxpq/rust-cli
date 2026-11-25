//! 回合的文件改动（session-lifecycle.md §10.42）：回合任务在主循环之后读这个回合的 checkpoint，`TurnFinished` 与回合记录带同一份，冷读同值。
//! 回合的文件 diff（§10.45）：查询给记录里的数字与同一个 diff 的 hunk，打开着与冷读同值。

use std::sync::Arc;

use rust_cli_base::line_diff::Hunk;
use rust_cli_kernel_api::ids::{ArtifactId, MachineName, MessageId, SessionId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{
    Command, FileChange, KernelEvent, SessionHandle, TurnFileChange, TurnFileDiff, TurnOrigin,
    TurnOutcome,
};
use rust_cli_kernel_api::store::{
    ArtifactPurpose, ArtifactRef, SessionStore, TurnRecord, WorkspaceCheckpoint,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use serde_json::json;
use tokio::sync::Notify;

use crate::rewind::tests::Writing;
use crate::test_support::{Setup, envelope, finished, submit, until};

fn write(
    id: &str,
    path: &str,
    content: &str,
) -> Vec<Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>> {
    tool_use_response(id, "Write", &json!({"file_path": path, "content": content}))
}

fn change(path: &str, (additions, deletions): (u64, u64), writes: u64) -> TurnFileChange {
    TurnFileChange {
        change: FileChange {
            path: path.to_owned(),
            additions,
            deletions,
            machine: MachineName::primary(),
        },
        writes,
        tools: vec!["Write".to_owned()],
    }
}

fn finished_files(events: &[KernelEvent]) -> (TurnOutcome, Vec<TurnFileChange>) {
    match events.last() {
        Some(KernelEvent::TurnFinished { outcome, files, .. }) => (outcome.clone(), files.clone()),
        other => panic!("not TurnFinished: {other:?}"),
    }
}

/// 一个回合写两个文件、其中一个写两次：每个文件最早的写入前 → 最后的写入后（不是每次写入加起来），次数与工具；按路径排。
/// 事件、视图里的回合记录与冷读的回合记录同值。以前回合结束不带文件改动，冷读给不出与活会话同一个回合头。
#[tokio::test]
async fn a_turn_reports_its_file_changes_once_and_the_record_keeps_them() {
    let model = Arc::new(ScriptedModel::new(vec![
        write("toolu_1", "/work/b.txt", "one\ntwo\n"),
        write("toolu_2", "/work/a.txt", "old\nnew\n"),
        write("toolu_3", "/work/b.txt", "one\n2\n"),
        text_response(&["done"]),
    ]));
    let env = crate::test_support::env();
    env.set_file("/work/a.txt", "old\n");
    let mut setup = Setup::new(model);
    setup.set_env(env);
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write them")).await;
    let seen = until(&mut events, finished).await;
    let expected = vec![
        change("/work/a.txt", (1, 0), 1),
        // 新文件写了两次：最早的写入前是空，最后是 `one\n2\n`（每次加起来会是 +3/−1）
        change("/work/b.txt", (2, 0), 2),
    ];
    let (outcome, files) = finished_files(&seen);
    assert_eq!(outcome, TurnOutcome::Success);
    assert_eq!(files, expected);
    let view = built.session.view().await.unwrap();
    let ended = |turns: &[rust_cli_kernel_api::store::TurnRecord]| {
        turns[0].ended.as_ref().map(|ended| ended.files.clone())
    };
    assert_eq!(ended(&view.turns), Some(expected.clone()));
    let loaded = built.store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(ended(&loaded.turns), Some(expected));
}

/// 中断的回合同样带它写过的文件（v4 的活会话没有、冷读有；rust 两边都有）。
#[tokio::test]
async fn an_interrupted_turn_still_reports_what_it_wrote() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![
            write("toolu_1", "/work/c.txt", "c\n"),
            text_response(&["never"]),
        ])
        .gated_at(1, gate),
    );
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write c")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::FileChanges { .. })
    })
    .await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    let seen = until(&mut events, finished).await;
    let (outcome, files) = finished_files(&seen);
    assert_eq!(outcome, TurnOutcome::Interrupted);
    assert_eq!(files, vec![change("/work/c.txt", (1, 0), 1)]);
}

/// 没有写文件的回合没有文件改动（事件与记录都是空的，记录里不写这个键）。
#[tokio::test]
async fn a_turn_without_writes_has_no_file_changes() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["hi"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    assert!(finished_files(&seen).1.is_empty());
}

/// 只差末尾换行（`a` → `a\n`）：最后一行带不带换行是两个不同的行，+1/−1（同 jsdiff 与 Node）。以前按 `\n` 切行、去掉最后一个换行，算成 +0/−0。
#[tokio::test]
async fn only_a_final_newline_counts_both_lines() {
    let model = Arc::new(ScriptedModel::new(vec![
        write("toolu_1", "/work/n.txt", "a\n"),
        text_response(&["done"]),
    ]));
    let env = crate::test_support::env();
    env.set_file("/work/n.txt", "a");
    let mut setup = Setup::new(model);
    setup.set_env(env);
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "add the newline")).await;
    let seen = until(&mut events, finished).await;
    assert_eq!(
        finished_files(&seen).1,
        vec![change("/work/n.txt", (1, 1), 1)]
    );
}

fn message(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: Vec::new(),
        origin: UserOrigin::Human,
    })
}

fn turn_record(turn: &str, first: &str) -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(turn),
        first: MessageId::new(first),
        origin: TurnOrigin::Input,
        started_at: 0,
        command: None,
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

fn checkpoint(id: &str, prompt: &str, assistant: &str) -> WorkspaceCheckpoint {
    WorkspaceCheckpoint {
        id: id.to_owned(),
        prompt: MessageId::new(prompt),
        assistant: MessageId::new(assistant),
        tool_use: ToolUseId::new(id),
        snapshot: ArtifactRef {
            session: SessionId::new("sess_1"),
            id: ArtifactId::new(id),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: "application/json".to_owned(),
            size: 0,
        },
        machine: MachineName::primary(),
    }
}

/// 这个回合的 checkpoint 按发出写入的 assistant 消息取（§10.45）：显示顺序里从回合记录的第一条消息起、到下一个回合的第一条之前。
/// 开回合的消息不在历史里（斜杠命令的展开没留它）也取得到；不认识的回合为空。
#[test]
fn a_turn_owns_the_checkpoints_of_its_assistant_messages() {
    let messages: Vec<Message> = ["u1", "a1", "r1", "a1b", "u2", "a2", "r2"]
        .into_iter()
        .map(message)
        .collect();
    let turns = [turn_record("turn_1", "u1"), turn_record("turn_2", "u2")];
    let checkpoints = [
        checkpoint("c1", "u1", "a1"),
        checkpoint("c2", "msg_expanded", "a1b"),
        checkpoint("c3", "u2", "a2"),
    ];
    let ids = |turn: &str| -> Vec<String> {
        super::of_turn(&messages, &turns, &checkpoints, &TurnId::new(turn))
            .into_iter()
            .map(|checkpoint| checkpoint.id)
            .collect()
    };
    assert_eq!(ids("turn_1"), ["c1", "c2"]);
    assert_eq!(ids("turn_2"), ["c3"]);
    assert!(ids("turn_9").is_empty());
}

fn artifact(path: &str, before: Option<&str>, after: &str) -> crate::rewind_plan::Artifact {
    crate::rewind_plan::Artifact {
        files: vec![crate::rewind_plan::ArtifactFile {
            path: path.into(),
            existed_before: before.is_some(),
            before_content: before.map(str::to_owned),
            after_content: Some(after.into()),
        }],
        tool_name: "Write".into(),
    }
}

/// 同一（机器，路径）合在一起：最早的写入前 → 最后的写入后，次数与工具；两台机器上的同名文件分开，主 env 在前。
#[test]
fn writes_add_up_per_machine_and_path() {
    let remote = MachineName::new("box");
    let written = super::accumulate(&[
        (
            MachineName::primary(),
            artifact("/b", Some("1\n2\n3\n"), "1\nx\n3\n"),
        ),
        (remote.clone(), artifact("/b", None, "r\n")),
        (
            MachineName::primary(),
            artifact("/b", Some("1\nx\n3\n"), "1\nx\n3\n4\n"),
        ),
    ]);
    assert_eq!(written.len(), 2);
    assert_eq!(
        (
            &written[0].machine,
            written[0].before.as_str(),
            written[0].after.as_str(),
            written[0].writes
        ),
        (&MachineName::primary(), "1\n2\n3\n", "1\nx\n3\n4\n", 2)
    );
    assert_eq!(
        super::diff(&written[0].before, &written[0].after).0,
        2,
        "+x and +4"
    );
    assert_eq!(super::diff(&written[0].before, &written[0].after).1, 1);
    assert_eq!((&written[1].machine, written[1].writes), (&remote, 1));
}

/// 编辑距离超过上限：没有 hunk，增删行数是去掉公共首尾行之后两边剩下的行数（与以前相同）。
#[test]
fn a_diff_past_the_edit_limit_has_counts_without_hunks() {
    let big: String = (1..=10_001).map(|n| format!("{n}\n")).collect();
    let (additions, deletions, hunks) = super::diff("", &big);
    assert_eq!((additions, deletions), (10_001, 0));
    assert!(hunks.is_empty());
    let edited = format!("head\n{big}tail\n");
    let replaced: String = (1..=10_001).map(|n| format!("x{n}\n")).collect();
    let (additions, deletions, hunks) = super::diff(&edited, &format!("head\n{replaced}tail\n"));
    assert_eq!((additions, deletions), (10_001, 10_001));
    assert!(hunks.is_empty());
}

fn turn_of(events: &[KernelEvent]) -> TurnId {
    events
        .iter()
        .find_map(|event| match event {
            KernelEvent::TurnStarted { turn, .. } => Some(turn.clone()),
            _ => None,
        })
        .expect("TurnStarted")
}

fn hunk(old: (usize, usize), new: (usize, usize), lines: &[&str]) -> Hunk {
    Hunk {
        old_start: old.0,
        old_lines: old.1,
        new_start: new.0,
        new_lines: new.1,
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
    }
}

/// 查询的回合文件 diff（§10.45）：数字是回合记录里的那一份，hunk 是同一组 artifact 的 jsdiff hunk（上下文 4 行，最早的写入前 → 最后的写入后）；
/// 存储给的状态（冷读）同值；不认识的回合为空。
#[tokio::test]
async fn the_turn_file_diffs_carry_the_record_and_the_hunks() {
    let model = Arc::new(ScriptedModel::new(vec![
        write("toolu_1", "/work/b.txt", "one\ntwo\n"),
        write("toolu_2", "/work/a.txt", "old\nnew\n"),
        write("toolu_3", "/work/b.txt", "one\n2\n"),
        text_response(&["done"]),
    ]));
    let env = crate::test_support::env();
    env.set_file("/work/a.txt", "old\n");
    let mut setup = Setup::new(model);
    setup.set_env(env);
    setup.tools = vec![Arc::new(Writing)];
    let artifacts = setup.artifacts.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write them")).await;
    let seen = until(&mut events, finished).await;
    let turn = turn_of(&seen);
    let open = built.session.turn_file_diffs(&turn).await.unwrap();
    assert_eq!(
        open,
        vec![
            TurnFileDiff {
                file: change("/work/a.txt", (1, 0), 1),
                hunks: vec![hunk((1, 1), (1, 2), &[" old", "+new"])],
            },
            TurnFileDiff {
                file: change("/work/b.txt", (2, 0), 2),
                hunks: vec![hunk((1, 0), (1, 2), &["+one", "+2"])],
            },
        ]
    );
    let loaded = built.store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(
        crate::turn_file_diffs(artifacts.as_ref(), &loaded, &turn).await,
        open
    );
    let unknown = built
        .session
        .turn_file_diffs(&TurnId::new("turn_x"))
        .await
        .unwrap();
    assert!(unknown.is_empty());
}

/// 回合还在跑时没有文件 diff（记录没有结局，同回合头）；中断之后有它写过的文件。
#[tokio::test]
async fn a_running_turn_has_no_file_diffs_yet() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![
            write("toolu_1", "/work/c.txt", "c\n"),
            text_response(&["never"]),
        ])
        .gated_at(1, gate),
    );
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write c")).await;
    let written = until(&mut events, |event| {
        matches!(event, KernelEvent::FileChanges { .. })
    })
    .await;
    let turn = turn_of(&written);
    assert!(
        built
            .session
            .turn_file_diffs(&turn)
            .await
            .unwrap()
            .is_empty()
    );
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, finished).await;
    let ended = built.session.turn_file_diffs(&turn).await.unwrap();
    assert_eq!(
        ended,
        vec![TurnFileDiff {
            file: change("/work/c.txt", (1, 0), 1),
            hunks: vec![hunk((1, 0), (1, 1), &["+c"])],
        }]
    );
}

/// 大文件（编辑距离超过上限）：明细照回合记录有它，hunk 为空。
#[tokio::test]
async fn a_file_past_the_edit_limit_has_no_hunks() {
    let big: String = (1..=10_001).map(|n| format!("{n}\n")).collect();
    let model = Arc::new(ScriptedModel::new(vec![
        write("toolu_1", "/work/big.txt", &big),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write big")).await;
    let seen = until(&mut events, finished).await;
    let diffs = built
        .session
        .turn_file_diffs(&turn_of(&seen))
        .await
        .unwrap();
    assert_eq!(
        diffs,
        vec![TurnFileDiff {
            file: change("/work/big.txt", (10_001, 0), 1),
            hunks: Vec::new(),
        }]
    );
}
