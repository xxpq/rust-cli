use super::*;
use crate::env::EnvPath;
use crate::ids::EnvironmentId;

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

fn session(id: &str) -> SessionId {
    SessionId::new(id)
}

fn file(path: &str, artifact: bool, bytes: u64) -> SessionFile {
    SessionFile {
        relative_path: path.to_owned(),
        name: path.rsplit('/').next().unwrap_or(path).to_owned(),
        artifact,
        bytes,
    }
}

fn titled(id: &str) -> TitledSession {
    TitledSession {
        session: session(id),
        title: id.to_owned(),
    }
}

/// 一棵：根 beta 与子代理 child；fork 引用了 beta 的附件 shared。
fn scope() -> DeletionScope {
    DeletionScope {
        sessions: vec![session("beta"), session("child")],
        workspace: Some(workspace("/w")),
        forks: vec![titled("fork")],
        references: vec![ArtifactReference {
            session: session("beta"),
            artifact: "shared".to_owned(),
            referrers: vec![titled("fork")],
        }],
        stamps: vec![
            SessionStamp {
                session: session("beta"),
                updated_ms: 10,
                deleted_ms: None,
            },
            SessionStamp {
                session: session("child"),
                updated_ms: 1,
                deleted_ms: None,
            },
        ],
        pending_work: false,
        receipt: None,
    }
}

fn files() -> Vec<(SessionId, Vec<SessionFile>)> {
    vec![
        (
            session("beta"),
            vec![
                file("cli/artifacts/beta/private.png", true, 3),
                file("cli/artifacts/beta/shared.png", true, 5),
                file("tasks/beta.json", false, 2),
            ],
        ),
        (session("child"), Vec::new()),
    ]
}

/// 被引用的附件按文件名含 artifact id 对上文件、留下；其余的文件都删（同 Node）。
#[test]
fn a_referenced_attachment_is_kept_and_the_rest_is_counted() {
    let preview = preview(&workspace("/w"), &scope(), &files(), false);
    assert_eq!(preview.sessions, [session("beta"), session("child")]);
    assert_eq!(preview.forks, [titled("fork")]);
    assert_eq!(preview.retained.len(), 1);
    assert_eq!(preview.retained[0].file_name, "shared.png");
    assert_eq!(preview.retained[0].referrers, [titled("fork")]);
    assert_eq!(preview.deleted_files, 2);
    assert_eq!(preview.blocked, None);
    assert!(!preview.completed);
    assert_eq!(preview.kept_files(&session("beta")), ["shared"]);
    assert!(preview.kept_files(&session("child")).is_empty());
}

/// 引用对不上文件时文件名记 artifact id（同 Node），不少算删掉的文件。
#[test]
fn an_unmatched_reference_names_the_artifact_id() {
    let mut scope = scope();
    scope.references[0].artifact = "gone".to_owned();
    let preview = preview(&workspace("/w"), &scope, &files(), false);
    assert_eq!(preview.retained[0].file_name, "gone");
    assert_eq!(preview.deleted_files, 3);
}

/// revision 只随事实变：同样的输入同一个值；改了一个会话、多了一个文件、阻止了，都换值。
#[test]
fn the_revision_follows_the_facts() {
    let base = preview(&workspace("/w"), &scope(), &files(), false).revision;
    assert_eq!(
        preview(&workspace("/w"), &scope(), &files(), false).revision,
        base
    );
    let mut touched = scope();
    touched.stamps[0].updated_ms = 11;
    assert_ne!(
        preview(&workspace("/w"), &touched, &files(), false).revision,
        base
    );
    let mut more = files();
    more[1].1.push(file("cli/exec/child/out.log", false, 1));
    assert_ne!(
        preview(&workspace("/w"), &scope(), &more, false).revision,
        base
    );
    assert_ne!(
        preview(&workspace("/w"), &scope(), &files(), true).revision,
        base
    );
}

/// 在用（打开着的会话，或库里 Node 写的待处理工作）阻止（PD-D3）；根在别的工作区优先于它。
#[test]
fn work_in_progress_or_another_workspace_blocks() {
    assert_eq!(
        preview(&workspace("/w"), &scope(), &files(), true).blocked,
        Some(DeletionBlock::Running)
    );
    let mut pending = scope();
    pending.pending_work = true;
    assert_eq!(
        preview(&workspace("/w"), &pending, &files(), false).blocked,
        Some(DeletionBlock::Running)
    );
    assert_eq!(
        preview(&workspace("/other"), &scope(), &files(), true).blocked,
        Some(DeletionBlock::OtherWorkspace)
    );
}

/// 根不在库里、也没有凭据：同 Node，树为空 → 不能删（not-found）。
#[test]
fn a_missing_root_without_a_receipt_is_not_found() {
    let preview = preview(&workspace("/w"), &DeletionScope::default(), &[], false);
    assert!(preview.sessions.is_empty());
    assert_eq!(preview.blocked, Some(DeletionBlock::NotFound));
    assert!(!preview.completed && !preview.incomplete);
}

/// 凭据（Node `session_purge`）：有 deleting → 删到一半；全是 deleted → 删完。
#[test]
fn the_receipt_says_incomplete_or_completed() {
    let mut deleting = scope();
    deleting.receipt = Some(Receipt::Deleting);
    let preview_deleting = preview(&workspace("/w"), &deleting, &files(), false);
    assert!(preview_deleting.incomplete && !preview_deleting.completed);
    assert_eq!(preview_deleting.blocked, None);
    let mut deleted = scope();
    deleted.receipt = Some(Receipt::Deleted);
    let preview_deleted = preview(&workspace("/w"), &deleted, &[], false);
    assert!(preview_deleted.completed && !preview_deleted.incomplete);
}
