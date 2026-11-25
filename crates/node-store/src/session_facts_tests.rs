//! 会话列表的事实（docs/specs/rust-cli/session-facts.md §2）：取自会话行与它的配置记录；按 id 查、最近的列表会话。
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_facts::{MigrationSource, SessionOutcome, TitleSource};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionStore, StoreQueries};

use crate::tests::{TestStore, batch, meta, selection, started, user};

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

fn workspace(root: &str) -> rust_cli_kernel_api::env::WorkspaceRef {
    meta("sess_x", root, "").workspace
}

#[tokio::test]
async fn the_facts_come_from_the_session_row_and_its_config() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(5000);
    t.store
        .commit(
            &sid("sess_1"),
            batch(None, vec![user("msg_lz1ke0_a", "Hello there")]),
        )
        .await
        .unwrap();
    t.source.at(6000);
    let ended = Fact::TurnEnded {
        status: CallStatus::Cancelled,
    };
    t.store
        .commit(&sid("sess_1"), batch(None, vec![ended]))
        .await
        .unwrap();
    let found = t.store.session_facts(&[sid("sess_1")]).await.unwrap();
    let [facts] = found.as_slice() else {
        panic!("{found:?}")
    };
    assert_eq!(facts.id, sid("sess_1"));
    assert_eq!(facts.kind, SessionKind::Interactive);
    assert_eq!(facts.parent, None);
    assert_eq!(facts.workspace, workspace("/w"));
    // 首条输入的标题（Node `titleFromInput`），来源 first_input。
    assert_eq!(facts.title, "Hello there");
    assert_eq!(facts.title_source, TitleSource::FirstInput);
    assert_eq!(facts.created_at_ms, 1000);
    assert_eq!(facts.updated_at_ms, 6000);
    assert_eq!(facts.last_outcome, Some(SessionOutcome::Interrupted));
    assert_eq!(facts.deleted_at_ms, None);
    // 首条输入的消息行的创建时间。
    let created =
        t.rows("select time_created from message where id = 'msg_lz1ke0_a'")[0][0].as_u64();
    assert_eq!(facts.first_input_at_ms, created);
    assert_eq!(facts.migration, None);
    assert_eq!(facts.selection, Some(selection()));
    assert_eq!(
        facts.execution,
        Some(ExecutionState::from_parts(PermissionMode::Default, false))
    );
    t.store.rename(&sid("sess_1"), "Mine").await.unwrap();
    t.source.at(7000);
    t.store.delete(&sid("sess_1")).await.unwrap();
    let facts = &t.store.session_facts(&[sid("sess_1")]).await.unwrap()[0];
    assert_eq!(
        (facts.title.as_str(), facts.title_source),
        ("Mine", TitleSource::Custom)
    );
    assert_eq!(facts.deleted_at_ms, Some(7000));
}

/// 按给出的顺序；库里没有的不给出；删除了的、子代理的子会话、别的目录的照样给出（调用方按 workspace 隔离）。
#[tokio::test]
async fn the_lookup_keeps_the_order_and_skips_unknown_ids() {
    let t = TestStore::new();
    t.store
        .create(meta("sess_a", "/w", ""), crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    t.store
        .create(
            meta("sess_b", "/elsewhere", ""),
            crate::tests::batch(None, vec![]),
        )
        .await
        .unwrap();
    let mut child = meta("sess_child", "/w", "");
    child.kind = SessionKind::Subagent;
    child.parent = Some(sid("sess_a"));
    t.store
        .create(child, crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    t.store.delete(&sid("sess_a")).await.unwrap();
    let found = t
        .store
        .session_facts(&[
            sid("sess_child"),
            sid("sess_missing"),
            sid("sess_b"),
            sid("sess_a"),
        ])
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|facts| facts.id.as_str()).collect();
    assert_eq!(ids, ["sess_child", "sess_b", "sess_a"]);
    assert_eq!(found[0].kind, SessionKind::Subagent);
    assert_eq!(found[0].parent, Some(sid("sess_a")));
    assert_eq!(found[1].workspace, workspace("/elsewhere"));
    assert!(found[2].deleted_at_ms.is_some());
    // 还没有输入、没有选过模型：没有首条输入时刻、没有模型；模式是建会话时写的 `permission.mode = build`（Node 同）。
    assert_eq!(found[1].first_input_at_ms, None);
    assert_eq!(found[1].selection, None);
    assert_eq!(
        found[1].execution,
        Some(ExecutionState::from_parts(PermissionMode::Default, false))
    );
    assert_eq!(found[1].title_source, TitleSource::Default);
}

/// 列表会话（`query_sessions` 的口径去掉“没删除”）：子代理的子会话、远端会话、别的目录不在；删除了的在；按活动时间从新到旧。
#[tokio::test]
async fn recent_facts_are_the_listed_sessions_including_deleted_ones() {
    let t = TestStore::new();
    for (index, id) in ["sess_a", "sess_b", "sess_c", "sess_d"]
        .into_iter()
        .enumerate()
    {
        t.store
            .create(meta(id, "/w", ""), crate::tests::batch(None, vec![]))
            .await
            .unwrap();
        t.source.at(9000 - index as u64 * 1000);
        t.store
            .commit(&sid(id), batch(None, vec![user(&format!("msg_{id}"), id)]))
            .await
            .unwrap();
    }
    let mut child = meta("sess_child", "/w", "");
    child.kind = SessionKind::Subagent;
    child.parent = Some(sid("sess_a"));
    t.store
        .create(child, crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    let mut fork = meta("sess_fork", "/w", "Fork of a");
    fork.kind = SessionKind::Fork;
    fork.parent = Some(sid("sess_a"));
    t.store
        .create(fork, crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    t.store
        .create(
            meta("sess_other", "/elsewhere", ""),
            crate::tests::batch(None, vec![]),
        )
        .await
        .unwrap();
    t.store.delete(&sid("sess_b")).await.unwrap();
    t.connection()
        .execute(
            "update session set workspace_id = 'remote:ssh:host:/w' where id = 'sess_c'",
            [],
        )
        .unwrap();
    let recent = t
        .store
        .recent_session_facts(&workspace("/w"), 3)
        .await
        .unwrap();
    let ids: Vec<&str> = recent.iter().map(|facts| facts.id.as_str()).collect();
    // sess_fork 的活动时间是创建时间 1000，最旧，超出上限。
    assert_eq!(ids, ["sess_a", "sess_b", "sess_d"]);
    assert!(recent[1].deleted_at_ms.is_some());
    let all = t
        .store
        .recent_session_facts(&workspace("/w"), 10)
        .await
        .unwrap();
    let ids: Vec<&str> = all.iter().map(|facts| facts.id.as_str()).collect();
    assert_eq!(ids, ["sess_a", "sess_b", "sess_d", "sess_fork"]);
}

/// 迁移来源看首条消息：Node 导入的固定消息 ID，或消息与 part 的 `metadata.migrationSource`（Node `isImportedHistoryMessage`）。
#[tokio::test]
async fn an_imported_history_marks_the_migration_source() {
    let t = TestStore::new();
    for id in [
        "sess_id",
        "sess_prefix",
        "sess_message",
        "sess_part",
        "sess_plain",
    ] {
        t.store
            .create(meta(id, "/w", ""), crate::tests::batch(None, vec![]))
            .await
            .unwrap();
    }
    let conn = t.connection();
    let message = |id: &str, session: &str, data: &str| {
        conn.execute(
            "insert into message (id, session_id, time_created, time_updated, data, sequence) values (?, ?, 1, 1, ?, 0)",
            [id, session, data],
        )
        .unwrap();
    };
    let plain = r#"{"role":"user","time":{"created":1}}"#;
    message("msg_import_7", "sess_id", plain);
    message("msg_claude-import-abc", "sess_prefix", plain);
    message(
        "msg_m",
        "sess_message",
        r#"{"role":"user","time":{"created":1},"metadata":{"migrationSource":"claudeCode"}}"#,
    );
    message("msg_p", "sess_part", plain);
    conn.execute(
        "insert into part (id, message_id, session_id, time_created, time_updated, data, sequence) values ('prt_1', 'msg_p', 'sess_part', 1, 1, ?, 0)",
        [r#"{"type":"text","text":"x","metadata":{"migrationSource":"claudeCode"}}"#],
    )
    .unwrap();
    message("msg_plain", "sess_plain", plain);
    let found = t
        .store
        .session_facts(
            &[
                "sess_id",
                "sess_prefix",
                "sess_message",
                "sess_part",
                "sess_plain",
            ]
            .map(sid),
        )
        .await
        .unwrap();
    let marks: Vec<_> = found.iter().map(|facts| facts.migration).collect();
    let imported = Some(MigrationSource::ImportedHistory);
    assert_eq!(marks, [imported, imported, imported, imported, None]);
}

/// Node 写的、本工程不认识的结局（`last_outcome` 是自由文本）：按 error（错了也安全：结局可见）。
#[tokio::test]
async fn an_unknown_outcome_reads_as_an_error() {
    let t = TestStore::new();
    t.store
        .create(meta("sess_1", "/w", ""), crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    t.connection()
        .execute(
            "update session set last_outcome = 'paused' where id = 'sess_1'",
            [],
        )
        .unwrap();
    let facts = &t.store.session_facts(&[sid("sess_1")]).await.unwrap()[0];
    assert_eq!(facts.last_outcome, Some(SessionOutcome::Error));
}
