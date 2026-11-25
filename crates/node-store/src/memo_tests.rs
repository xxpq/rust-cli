//! 按库的版本记住的读（node-store.md §5.7）：版本不同不给；同一会话记下新版本时旧版本的别的东西一起丢；超过上限丢最久没用的。

use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;

use super::{HISTORY_MEMO_SESSIONS, HistoryMemo, Version};

const V1: Version = Version {
    data: 1,
    changes: 0,
};
const V2: Version = Version {
    data: 1,
    changes: 1,
};

/// 一份索引（Node 写的夹具经真的范围读建出；测试只比指针）。
fn index() -> Arc<rust_cli_node_records::range::HistoryIndex> {
    let fixture = crate::hydrate_tests::fixture();
    let (_dir, conn) = crate::hydrate_tests::database(&fixture["basic"]["tables"]);
    let session = SessionId::new(fixture["basic"]["sessionID"].as_str().unwrap());
    let span = rust_cli_kernel_api::store::HistorySpan::Before {
        floor: None,
        records: 1,
    };
    rust_cli_node_records::range::read(&crate::rows::Sqlite(&conn), &session, &span, None)
        .unwrap()
        .index
}

#[test]
fn a_remembered_read_is_given_back_only_at_its_version() {
    let memo = HistoryMemo::default();
    let session = SessionId::new("sess_1");
    let kept = index();
    memo.remember_index(&session, V1, kept.clone());
    memo.remember_subagents(&session, V1, Vec::new());
    assert!(Arc::ptr_eq(&memo.index(&session, V1).unwrap(), &kept));
    assert_eq!(memo.subagents(&session, V1), Some(Vec::new()));
    assert!(memo.index(&session, V2).is_none());
    // 新版本下记下子代理：旧版本的索引不再给
    memo.remember_subagents(&session, V2, Vec::new());
    assert!(memo.index(&session, V2).is_none());
    assert!(memo.index(&session, V1).is_none());
    assert_eq!(memo.subagents(&session, V2), Some(Vec::new()));
}

#[test]
fn the_least_recently_used_session_goes_first() {
    let memo = HistoryMemo::default();
    let kept = index();
    let sessions: Vec<SessionId> = (0..=HISTORY_MEMO_SESSIONS)
        .map(|n| SessionId::new(format!("sess_{n}")))
        .collect();
    for session in &sessions[..HISTORY_MEMO_SESSIONS] {
        memo.remember_index(session, V1, kept.clone());
    }
    // 用一下第一个：它挪到最近
    assert!(memo.index(&sessions[0], V1).is_some());
    memo.remember_index(&sessions[HISTORY_MEMO_SESSIONS], V1, kept);
    assert!(memo.index(&sessions[0], V1).is_some());
    assert!(memo.index(&sessions[1], V1).is_none(), "最久没用的丢了");
}
