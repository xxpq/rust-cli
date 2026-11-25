//! 草稿落库（docs/specs/rust-cli/session-facts.md §4）：`Fact::SessionCreated` 在一批的第一项建会话行，与这一批同一个事务；
//! 写出的行与先 `create` 再 `commit` 的相同（Node 读到的一样）。
use futures_util::StreamExt;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, SessionStore, StoreError, StoreQueries};

use crate::tests::{TestStore, batch, meta, selection, user};

const TABLES: [&str; 4] = [
    "select * from session",
    "select * from message",
    "select * from part",
    "select * from session_entry order by id",
];

fn config() -> Vec<Fact> {
    vec![
        Fact::ModelSelected {
            selection: selection(),
        },
        Fact::ModeChanged {
            mode: PermissionMode::AcceptEdits,
        },
    ]
}

#[tokio::test]
async fn a_draft_is_stored_with_its_first_batch_like_create_then_commit() {
    let draft = TestStore::new();
    let mut changes = draft.store.session_fact_changes();
    let mut facts = vec![Fact::SessionCreated {
        meta: meta("sess_1", "/w", ""),
    }];
    facts.extend(config());
    facts.push(user("msg_1_u1", "Hello"));
    draft
        .store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await
        .unwrap();
    let stored = changes.next().await.unwrap();
    assert_eq!(
        (stored.title.as_str(), stored.selection),
        ("Hello", Some(selection()))
    );

    let two_steps = TestStore::new();
    two_steps
        .store
        .create(meta("sess_1", "/w", ""), crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    let mut facts = config();
    facts.push(user("msg_1_u1", "Hello"));
    two_steps
        .store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await
        .unwrap();
    for table in TABLES {
        assert_eq!(draft.rows(table), two_steps.rows(table), "{table}");
    }
}

/// 会话行只由一批的第一项建：别的位置是错的批次，整批不写。
#[tokio::test]
async fn a_session_row_is_created_only_by_the_first_fact() {
    let t = TestStore::new();
    let facts = vec![
        user("msg_1_u1", "Hello"),
        Fact::SessionCreated {
            meta: meta("sess_1", "/w", ""),
        },
    ];
    let refused = t
        .store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await;
    assert!(
        matches!(refused, Err(StoreError::NotFound { .. })),
        "{refused:?}"
    );
    t.store
        .create(meta("sess_2", "/w", ""), crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    let late = vec![
        user("msg_2_u1", "Hello"),
        Fact::SessionCreated {
            meta: meta("sess_2", "/w", ""),
        },
    ];
    let refused = t
        .store
        .commit(&SessionId::new("sess_2"), batch(None, late))
        .await;
    assert!(
        matches!(refused, Err(StoreError::Storage { .. })),
        "{refused:?}"
    );
    assert!(t.rows("select id from message").is_empty());
}
