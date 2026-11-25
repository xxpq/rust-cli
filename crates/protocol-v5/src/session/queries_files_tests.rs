//! `session.fileChanges` 的明细（protocol-v5.md §44）：回合头在这一步判定、交出回合 id 与 cursor；数字取回合记录、`patches` 在预算之内按顺序放；
//! 物化了的与没有物化的会话都经任务读内核。

use rust_cli_base::line_diff::Hunk;
use rust_cli_kernel_api::ids::MachineName;
use rust_cli_kernel_api::session_api::{FileChange, TurnFileChange};

use super::*;

/// 文件改动只认回合头（§40.2）：判定在这一步，回合头 → 明细交给内核（§44.2，cursor 是这一刻的）；别的行 → invalid，不在 → notFound，
/// `at` 过期 → stale（不读明细）。以前按回合头上的明细在这一步答，`patches` 为空。
#[test]
fn file_changes_name_a_turn_header() {
    let publisher = published(&turns(2));
    let changed = Answered::FileChanges {
        row: "turn_1".to_owned(),
    };
    match answer(&publisher, &complete(), &changed, None, SID).unwrap() {
        Answer::Kernel {
            read: KernelRead::FileDiffs { turn, reverted },
            cursor,
        } => {
            assert_eq!(turn, TurnId::new("turn_1"));
            assert_eq!(cursor, publisher.cursor());
            assert!(!reverted);
        }
        other => panic!("{other:?}"),
    }
    let other = Cursor {
        epoch: "other".to_owned(),
        seq: 0,
    };
    let stale = ready(&publisher, &changed, Some(&other), SID).unwrap();
    assert_eq!(stale["kind"], json!("stale"));
    let user_row = Answered::FileChanges {
        row: "msg_u1".to_owned(),
    };
    assert_eq!(
        answer(&publisher, &complete(), &user_row, None, SID)
            .unwrap_err()
            .code,
        codes::PRECONDITION_INVALID
    );
    let missing = Answered::FileChanges {
        row: "turn_9".to_owned(),
    };
    assert_eq!(
        answer(&publisher, &complete(), &missing, None, SID)
            .unwrap_err()
            .code,
        codes::PRECONDITION_NOT_FOUND
    );
}

fn diff(path: &str, (additions, deletions): (u64, u64), hunks: Vec<Hunk>) -> TurnFileDiff {
    TurnFileDiff {
        file: TurnFileChange {
            change: FileChange {
                path: path.to_owned(),
                additions,
                deletions,
                machine: MachineName::primary(),
            },
            writes: 1,
            tools: vec!["Write".to_owned()],
        },
        hunks,
    }
}

fn added(lines: &[&str]) -> Hunk {
    Hunk {
        old_start: 1,
        old_lines: 0,
        new_start: 1,
        new_lines: lines.len(),
        lines: lines.iter().map(|line| format!("+{line}")).collect(),
    }
}

/// 明细换成契约的形状（§44.2）：数字取回合记录的，hunk 换成 `DiffHunkSchema`；汇总由明细算。没有文件时是空的汇总。
#[test]
fn file_changes_carry_the_record_and_the_patches() {
    let data = json!(file_changes(
        &[
            diff(
                &format!("{KEY}/a.txt"),
                (2, 0),
                vec![added(&["one", "two"])]
            ),
            diff(&format!("{KEY}/big.txt"), (10_001, 0), Vec::new()),
        ],
        FILE_PATCHES_MAX_BYTES
    ));
    assert_eq!(
        data,
        json!({"files": 2, "additions": 10_003, "deletions": 0, "state": "active", "items": [
            {"path": format!("{KEY}/a.txt"), "additions": 2, "deletions": 0, "writeCount": 1,
             "toolNames": ["Write"], "patches": [
                {"oldStart": 1.0, "oldLines": 0.0, "newStart": 1.0, "newLines": 2.0, "lines": ["+one", "+two"]}
             ]},
            {"path": format!("{KEY}/big.txt"), "additions": 10_001, "deletions": 0, "writeCount": 1,
             "toolNames": ["Write"], "patches": []}
        ]})
    );
    assert_eq!(
        json!(file_changes(&[], FILE_PATCHES_MAX_BYTES)),
        json!({"files": 0, "additions": 0, "deletions": 0, "items": []})
    );
}

/// 预算（§44.2）：按顺序放，放不下的那个文件 `patches` 为空，后面放得下的照给；数字不受影响。
#[test]
fn patches_past_the_budget_are_left_out_in_order() {
    let long = "x".repeat(64);
    let hunk = || added(&[long.as_str()]);
    let bytes = |hunk: &Hunk| json!(vec![rows::file_patch(hunk)]).to_string().len();
    let small = added(&["s"]);
    let (one, little) = (bytes(&hunk()), bytes(&small));
    assert!(little < one);
    let diffs = [
        diff("/a", (1, 0), vec![hunk()]),
        diff("/b", (1, 0), vec![hunk()]),
        diff("/c", (1, 0), vec![small]),
    ];
    // 放得下第一个与最后一个小的，放不下第二个
    let result = file_changes(&diffs, one + little);
    let kept: Vec<usize> = result.items.iter().map(|item| item.patches.len()).collect();
    assert_eq!(kept, [1, 0, 1]);
    assert_eq!(result.additions, 3);
}

/// `turn_1` 的明细：一个新写的文件。
fn one_file() -> Vec<TurnFileDiff> {
    vec![diff(
        &format!("{KEY}/a.txt"),
        (2, 0),
        vec![added(&["one", "two"])],
    )]
}

/// 期望的明细里那个文件的 `patches`。
fn one_file_patches() -> Value {
    json!([{"oldStart": 1.0, "oldLines": 0.0, "newStart": 1.0, "newLines": 2.0, "lines": ["+one", "+two"]}])
}

/// 物化了的会话的文件改动（§44.2）：判定在这一步，明细不在这一步答，交给任务经内核读；结果经 `Event::SessionRead` 回来、带判定那一刻的 cursor；
/// 在途计数加一（`shutdown` 等它）。不是回合头的照旧在这一步拒绝。
#[tokio::test]
async fn file_changes_of_a_materialized_session_are_read_in_a_task() {
    let scope = TaskScope::root("test", |_| {});
    let options = Options {
        framing: Framing::ContentLength,
        dialect: Dialect::JsonRpc2,
        ..Options::default()
    };
    let (out, _host) = duplex(1 << 20);
    let (peer, _writer) = connection::start(out, &scope.spawner(), options);
    let (events, mut inbox) = mpsc::unbounded_channel();
    let mut actor = Actor::new(peer, Settings::default(), &scope.spawner(), events);
    actor.process_ready(
        local_with(turns(2), one_file(), None),
        Arc::new(MemoryStore::default()),
    );
    actor.lane(KEY, SID).unwrap();
    let generation = actor.sessions.slots[SID].lane.as_ref().unwrap().1;
    let viewed = LaneEvent::Viewed {
        view: Box::new(turns(2)),
        origin: origin(),
    };
    actor.on_lane(SID, generation, viewed);
    let envelope = |row: &str| -> QueryEnvelope {
        serde_json::from_value(json!({"type": SESSION_FILE_CHANGES,
            "target": {"hostId": "local", "workspaceKey": KEY, "sessionId": SID},
            "params": {"rowId": row}}))
        .unwrap()
    };
    let invalid = actor
        .session_query(&json!(1), SessionQuery::FileChanges, envelope("msg_u1"))
        .unwrap_err();
    assert_eq!(invalid.code, codes::PRECONDITION_INVALID);
    let reads = actor.sessions.reads;
    let answered = actor
        .session_query(&json!(2), SessionQuery::FileChanges, envelope("turn_1"))
        .unwrap();
    assert!(answered.is_none(), "{answered:?}");
    assert_eq!(actor.sessions.reads, reads + 1);
    let cursor = actor.topics[&session_topic(KEY, SID)].cursor();
    let result = loop {
        match inbox.recv().await.unwrap() {
            Event::SessionRead { reply, result } => {
                assert_eq!(reply, json!(2));
                break result.unwrap();
            }
            _ => continue,
        }
    };
    assert_eq!(result["cursor"], json!(cursor));
    let data = ok_data(&result);
    assert_eq!(data["files"], json!(1));
    assert_eq!(data["items"][0]["patches"], one_file_patches());
}

/// 没有物化的会话的文件改动（§44.2）：读视图的任务在临时投影上判定回合头，再经同一个内核读明细。
#[tokio::test]
async fn file_changes_of_a_session_without_a_topic_come_with_patches() {
    let mut h = Harness::new();
    h.handle.ready(
        local_with(turns(2), one_file(), None),
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    let mut frames = Vec::new();
    h.send(query_message(
        2,
        SESSION_FILE_CHANGES,
        json!({"rowId": "turn_1"}),
        None,
    ))
    .await;
    let reply = h.reply(2, &mut frames).await;
    let data = ok_data(&reply["result"]);
    assert_eq!(data["items"][0]["path"], json!(format!("{KEY}/a.txt")));
    assert_eq!(data["items"][0]["patches"], one_file_patches());
    let epoch = reply["result"]["cursor"]["epoch"].as_str().unwrap();
    assert!(epoch.contains(READ_EPOCH), "{epoch}");
    // 没有改动的回合头：空的汇总
    h.send(query_message(
        3,
        SESSION_FILE_CHANGES,
        json!({"rowId": "turn_2"}),
        None,
    ))
    .await;
    let empty = h.reply(3, &mut frames).await;
    assert_eq!(
        ok_data(&empty["result"]),
        &json!({"files": 0, "additions": 0, "deletions": 0, "items": []})
    );
}
