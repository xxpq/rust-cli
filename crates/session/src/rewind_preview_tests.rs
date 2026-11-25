//! 回退文件的预览（session-lifecycle.md §10.53）：范围与模拟同回退，现在的内容按执行环境的同一个 hash 比；只读。

use rust_cli_kernel_api::env_rewind::{RestoreAction, content_hash};
use rust_cli_kernel_api::ids::{ArtifactId, MachineName, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::rewind_preview::{
    PreviewFile, PreviewState, RewindPreview, UnreadableCheckpoint,
};
use rust_cli_kernel_api::session_api::{Ack, Command, RewindScope, SessionHandle};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, WorkspaceCheckpoint};
use rust_cli_kernel_api::testing::MemoryArtifacts;

use crate::rewind::tests::{file, wrote};
use crate::test_support::envelope;

fn a_txt(state: PreviewState) -> PreviewFile {
    PreviewFile {
        path: "/work/a.txt".to_owned(),
        machine: MachineName::primary(),
        action: RestoreAction::Restore,
        writes: 1,
        tools: vec!["Write".to_owned()],
        state,
    }
}

/// 写过之后没人动过：能恢复；预览不改文件、不改对话。
#[tokio::test]
async fn an_untouched_file_is_ready_and_nothing_changes() {
    let (built, env, prompt) = wrote().await;
    let preview = built.session.rewind_preview(&prompt).await.unwrap();
    assert_eq!(
        preview,
        Some(RewindPreview {
            files: vec![a_txt(PreviewState::Ready)],
            unreadable: Vec::new(),
        })
    );
    assert_eq!(file(&env, "/work/a.txt").as_deref(), Some("new\n"));
    assert_eq!(built.session.view().await.unwrap().messages.len(), 4);
    // 预览没有登记什么：之后照常回退
    let rewind = Command::Rewind {
        target: prompt,
        scope: RewindScope::Code,
    };
    assert!(matches!(
        built.session.send(envelope("r1", rewind)).await,
        Ack::Accepted { .. }
    ));
}

/// 写过之后被改过、被删掉：期望与现在的 hash（删掉是 None），同回退会报的冲突。
#[tokio::test]
async fn a_file_changed_or_removed_since_is_reported() {
    let (built, env, prompt) = wrote().await;
    env.set_file("/work/a.txt", "edited by hand\n");
    let preview = built
        .session
        .rewind_preview(&prompt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        preview.files,
        [a_txt(PreviewState::Changed {
            expected: Some(content_hash(b"new\n")),
            current: Some(content_hash(b"edited by hand\n")),
        })]
    );
    env.remove_file("/work/a.txt");
    let preview = built
        .session
        .rewind_preview(&prompt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        preview.files[0].state,
        PreviewState::Changed {
            expected: Some(content_hash(b"new\n")),
            current: None,
        }
    );
}

/// 不是可选的输入 → None；回退过之后再预览：没有要恢复的。
#[tokio::test]
async fn a_missing_target_and_an_already_rewound_turn() {
    let (built, _env, prompt) = wrote().await;
    assert_eq!(
        built
            .session
            .rewind_preview(&MessageId::new("msg_none"))
            .await
            .unwrap(),
        None
    );
    let rewind = Command::Rewind {
        target: prompt.clone(),
        scope: RewindScope::Code,
    };
    built.session.send(envelope("r1", rewind)).await;
    assert_eq!(
        built.session.rewind_preview(&prompt).await.unwrap(),
        Some(RewindPreview::default())
    );
}

/// 没打开的会话（冷）：同一个函数；读不出的 checkpoint 另列。
#[tokio::test]
async fn a_cold_preview_lists_unreadable_checkpoints() {
    let (built, _env, prompt) = wrote().await;
    let view = built.session.view().await.unwrap();
    let missing = WorkspaceCheckpoint {
        id: "checkpoint_missing".to_owned(),
        prompt: prompt.clone(),
        assistant: view.messages[1].id().clone(),
        tool_use: ToolUseId::new("toolu_x"),
        snapshot: ArtifactRef {
            session: SessionId::new("sess_1"),
            id: ArtifactId::new("art_missing"),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: "application/json".to_owned(),
            size: 0,
        },
        machine: MachineName::primary(),
    };
    let env = crate::test_support::env();
    let machines = rust_cli_kernel_api::env_registry::PrimaryRegistry::new(
        rust_cli_kernel_api::testing::machine(env.clone()),
        rust_cli_kernel_api::testing::controller_services(env),
    );
    let preview = crate::rewind_preview(
        &MemoryArtifacts::default(),
        &machines,
        &crate::test_support::meta().workspace,
        &view.messages,
        &[missing],
        &prompt,
    )
    .await
    .unwrap();
    assert!(preview.files.is_empty());
    assert!(matches!(
        preview.unreadable.as_slice(),
        [UnreadableCheckpoint { checkpoint, .. }] if checkpoint == "checkpoint_missing"
    ));
}
