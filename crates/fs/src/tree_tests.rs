//! 目录树的复制与遍历（plugin-store.md §5、§6）。

use std::path::PathBuf;

use super::*;

#[tokio::test]
async fn copy_tree_keeps_files_dirs_links_and_the_executable_bit() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("from");
    write_file(&from.join("a/b.txt"), b"bee").await.unwrap();
    write_file(&from.join("run.sh"), b"#!/bin/sh\n")
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(from.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::os::unix::fs::symlink("a/b.txt", from.join("link")).unwrap();
    }
    let to = dir.path().join("to");
    copy_tree(&from, &to, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(to.join("a/b.txt")).unwrap(), b"bee");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(to.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
        assert_eq!(
            std::fs::read_link(to.join("link")).unwrap(),
            PathBuf::from("a/b.txt")
        );
    }
    let entries = walk(&to).await.unwrap();
    let paths: Vec<_> = entries.iter().map(|entry| entry.path.clone()).collect();
    let mut expected = vec![
        PathBuf::from("a"),
        PathBuf::from("a/b.txt"),
        PathBuf::from("run.sh"),
    ];
    if cfg!(unix) {
        expected.insert(2, PathBuf::from("link"));
    }
    assert_eq!(paths, expected);
}

#[tokio::test]
async fn copy_tree_stops_when_cancelled_and_refuses_an_existing_target() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("from");
    write_file(&from.join("x"), b"x").await.unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = copy_tree(&from, &dir.path().join("to"), &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Interrupted);
    let error = copy_tree(&from, &from, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);
}
