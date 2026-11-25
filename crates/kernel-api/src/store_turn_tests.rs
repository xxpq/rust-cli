//! 回合记录的落库写法：结局的时刻、文件改动、执行覆盖的退回与开回合的后台工作写进 entry、读得回，较早写的记录照读（从 store_tests.rs 挪来）。

use super::*;

/// 回合记录的结局带结束的时刻与用时（session-lifecycle.md §10.36）：写进 entry、读回同值；较早写的记录（结局里没有 `time`）读成 None，不补。
#[test]
fn a_turn_record_keeps_when_the_turn_ended_and_old_records_have_no_time() {
    let record = TurnRecord {
        turn: TurnId::new("turn_1"),
        first: MessageId::new("msg_u1"),
        origin: crate::session_api::TurnOrigin::Input,
        started_at: 1_000,
        command: None,
        ended: Some(TurnEnded {
            outcome: crate::session_api::InputEnd::Completed,
            cancelled: Vec::new(),
            time: Some(crate::session_api::TurnTime {
                ended_at: 6_000,
                active_ms: 2_000,
            }),
            files: Vec::new(),
        }),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let Fact::Entry { id, data, .. } = record.fact() else {
        panic!("not an entry");
    };
    assert_eq!(
        data[TURN_ENDED_KEY]["time"],
        serde_json::json!({"endedAt": 6_000, "activeMs": 2_000})
    );
    assert_eq!(TurnRecord::from_entry(&id, &data), Some(record));
    let mut old = data;
    old[TURN_ENDED_KEY].as_object_mut().unwrap().remove("time");
    let read = TurnRecord::from_entry(&id, &old).unwrap();
    assert_eq!(read.ended.and_then(|ended| ended.time), None);
}

/// 回合记录的结局带这个回合的文件改动（session-lifecycle.md §10.42）：写进 entry、读回同值；较早写的记录没有它，读成空，不补。
#[test]
fn a_turn_record_keeps_the_file_changes_of_the_turn_and_old_records_have_none() {
    let change = crate::session_api::TurnFileChange {
        change: crate::session_api::FileChange {
            path: "/work/a.txt".to_owned(),
            additions: 2,
            deletions: 1,
            machine: crate::ids::MachineName::primary(),
        },
        writes: 2,
        tools: vec!["Edit".to_owned(), "Write".to_owned()],
    };
    let record = TurnRecord {
        turn: TurnId::new("turn_1"),
        first: MessageId::new("msg_u1"),
        origin: crate::session_api::TurnOrigin::Input,
        started_at: 1_000,
        command: None,
        ended: Some(TurnEnded {
            outcome: crate::session_api::InputEnd::Cancelled,
            cancelled: Vec::new(),
            time: None,
            files: vec![change],
        }),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let Fact::Entry { id, data, .. } = record.fact() else {
        panic!("not an entry");
    };
    assert_eq!(
        data[TURN_ENDED_KEY]["files"],
        serde_json::json!([{"path": "/work/a.txt", "additions": 2, "deletions": 1, "writes": 2, "tools": ["Edit", "Write"]}])
    );
    assert_eq!(TurnRecord::from_entry(&id, &data), Some(record));
    let mut old = data;
    old[TURN_ENDED_KEY].as_object_mut().unwrap().remove("files");
    let read = TurnRecord::from_entry(&id, &old).unwrap();
    assert!(read.ended.unwrap().files.is_empty());
}

/// 回合记录带执行覆盖的退回（session-lifecycle.md §10.48）：写进 entry 的 `fellBack`、读回同值；没有退回时不写这一项，较早写的记录读成 None。
#[test]
fn a_turn_record_keeps_its_fallback_and_old_records_have_none() {
    let record = TurnRecord {
        turn: TurnId::new("turn_1"),
        first: MessageId::new("msg_u1"),
        origin: crate::session_api::TurnOrigin::Input,
        started_at: 1_000,
        command: None,
        ended: None,
        fell_back: Some(crate::turn_execution::FellBack {
            reason: "highspeed_card_expired".to_owned(),
            at: 1_500,
        }),
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let Fact::Entry { id, data, .. } = record.fact() else {
        panic!("not an entry");
    };
    assert_eq!(
        data["fellBack"],
        serde_json::json!({"reason": "highspeed_card_expired", "at": 1_500})
    );
    assert_eq!(TurnRecord::from_entry(&id, &data), Some(record.clone()));
    let plain = TurnRecord {
        fell_back: None,
        ..record
    };
    let Fact::Entry { data, .. } = plain.fact() else {
        panic!("not an entry");
    };
    assert!(data.get("fellBack").is_none());
    assert_eq!(TurnRecord::from_entry(&id, &data), Some(plain));
}

/// 后台结果开的回合（session-lifecycle.md §10.50）：开它的后台工作写进 entry 的 `works`、读回同值，`start()` 带着它们（与 `TurnStarted` 同值）；
/// 别的回合不写这一项，较早写的记录读成空。
#[test]
fn a_turn_record_keeps_the_works_that_opened_it_and_old_records_have_none() {
    let works = vec![crate::background::NoticedWork {
        id: "b1".to_owned(),
        kind: crate::manage::BackgroundKind::Shell,
        description: "Print background marker".to_owned(),
    }];
    let record = TurnRecord {
        turn: TurnId::new("turn_n"),
        first: MessageId::new("msg_n1"),
        origin: crate::session_api::TurnOrigin::Notice,
        started_at: 1_000,
        command: None,
        ended: None,
        fell_back: None,
        works: works.clone(),
        compactions: Vec::new(),
    };
    let Fact::Entry { id, data, .. } = record.fact() else {
        panic!("not an entry");
    };
    assert_eq!(
        data["works"],
        serde_json::json!([{"id": "b1", "kind": "shell", "description": "Print background marker"}])
    );
    assert_eq!(TurnRecord::from_entry(&id, &data), Some(record.clone()));
    assert_eq!(record.start().works, works);
    let mut old = data;
    old.as_object_mut().unwrap().remove("works");
    assert!(TurnRecord::from_entry(&id, &old).unwrap().works.is_empty());
    let plain = TurnRecord {
        works: Vec::new(),
        ..record
    };
    let Fact::Entry { data, .. } = plain.fact() else {
        panic!("not an entry");
    };
    assert!(data.get("works").is_none());
}

/// 回合记录带这一轮主循环里的压缩（session-lifecycle.md §10.52）：写进 entry 的 `compactions`、读回同值；没有这类压缩的回合不写这一项，
/// 较早写的记录读成空。
#[test]
fn a_turn_record_keeps_its_main_loop_compactions_and_old_records_have_none() {
    let compactions = vec![
        crate::compaction::LoopCompaction {
            trigger: crate::message::CompactTrigger::Auto,
            started_at: 1_200,
            after: MessageId::new("msg_u1"),
            ended: Some(crate::compaction::LoopCompactionEnd::Failed),
        },
        crate::compaction::LoopCompaction {
            trigger: crate::message::CompactTrigger::Reactive,
            started_at: 1_300,
            after: MessageId::new("msg_u1"),
            ended: Some(crate::compaction::LoopCompactionEnd::Cancelled),
        },
    ];
    let record = TurnRecord {
        turn: TurnId::new("turn_1"),
        first: MessageId::new("msg_u1"),
        origin: crate::session_api::TurnOrigin::Input,
        started_at: 1_000,
        command: None,
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions,
    };
    let Fact::Entry { id, data, .. } = record.fact() else {
        panic!("not an entry");
    };
    assert_eq!(
        data[TURN_COMPACTIONS_KEY],
        serde_json::json!([
            {"trigger": "auto", "startedAt": 1_200, "after": "msg_u1", "ended": "failed"},
            {"trigger": "reactive", "startedAt": 1_300, "after": "msg_u1", "ended": "cancelled"},
        ])
    );
    assert_eq!(TurnRecord::from_entry(&id, &data), Some(record.clone()));
    let mut old = data;
    old.as_object_mut().unwrap().remove(TURN_COMPACTIONS_KEY);
    assert!(
        TurnRecord::from_entry(&id, &old)
            .unwrap()
            .compactions
            .is_empty()
    );
    let plain = TurnRecord {
        compactions: Vec::new(),
        ..record
    };
    let Fact::Entry { data, .. } = plain.fact() else {
        panic!("not an entry");
    };
    assert!(data.get(TURN_COMPACTIONS_KEY).is_none());
}
