//! 事实变化的发布端（session-facts.md §3.1）：按发布的顺序、每个订阅者都收到；订阅之前的不补；丢掉的订阅者去掉。
use futures_util::StreamExt;

use super::*;
use crate::env::EnvPath;
use crate::ids::EnvironmentId;

fn facts(id: &str, title: &str) -> SessionFacts {
    SessionFacts {
        id: SessionId::new(id),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/work"),
        },
        title: title.to_owned(),
        title_source: TitleSource::Custom,
        created_at_ms: 1,
        updated_at_ms: 2,
        last_outcome: None,
        deleted_at_ms: None,
        first_input_at_ms: None,
        migration: None,
        selection: None,
        execution: None,
    }
}

#[tokio::test]
async fn every_subscriber_gets_the_changes_in_order() {
    let changes = FactChanges::default();
    changes.publish(&facts("sess_before", "missed"));
    let mut first = changes.subscribe();
    let mut second = changes.subscribe();
    changes.publish(&facts("sess_a", "one"));
    changes.publish(&facts("sess_a", "two"));
    for stream in [&mut first, &mut second] {
        assert_eq!(stream.next().await.unwrap().title, "one");
        assert_eq!(stream.next().await.unwrap().title, "two");
    }
}

#[tokio::test]
async fn a_dropped_subscriber_is_removed() {
    let changes = FactChanges::default();
    let gone = changes.subscribe();
    let mut kept = changes.subscribe();
    drop(gone);
    changes.publish(&facts("sess_a", "one"));
    assert_eq!(changes.watchers.lock_unpoisoned().len(), 1);
    assert_eq!(kept.next().await.unwrap().title, "one");
}
