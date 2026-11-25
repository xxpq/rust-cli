#![allow(
    clippy::disallowed_methods,
    reason = "测试直接查看临时目录里解开的文件"
)]
use std::io::Write;

use super::*;

/// 现生成一个 zip：`(名字, 内容)`，内容为 None 是目录；`links` 是链接。
pub(crate) fn make_zip(entries: &[(&str, Option<&[u8]>)], links: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        match content {
            Some(bytes) => {
                writer.start_file(*name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
            None => writer.add_directory(*name, options).unwrap(),
        }
    }
    for (name, target) in links {
        writer.add_symlink(*name, *target, options).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
async fn extracts_files_and_strips_a_single_root() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = make_zip(
        &[
            ("kit-main/", None),
            (
                "kit-main/.claude-plugin/plugin.json",
                Some(br#"{"name":"kit"}"#),
            ),
            (
                "kit-main/skills/a/SKILL.md",
                Some(b"---\ndescription: A\n---\n"),
            ),
        ],
        &[],
    );
    let target = dir.path().join("x");
    let extracted = extract(bytes, &target, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(extracted.top_level.iter().collect::<Vec<_>>(), ["kit-main"]);
    let root = zip_root(&target, &extracted, None, None, true)
        .await
        .unwrap();
    assert_eq!(root, target.join("kit-main"));
    assert!(root.join("skills/a/SKILL.md").is_file());
    let sub = zip_root(&target, &extracted, Some("kit-main/skills"), None, false)
        .await
        .unwrap();
    assert_eq!(sub, target.join("kit-main").join("skills"));
    // stripRoot: false 时停在解开的目录。
    assert_eq!(
        zip_root(&target, &extracted, None, Some(false), false)
            .await
            .unwrap(),
        target
    );
}

#[tokio::test]
async fn unsafe_entries_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    for bytes in [
        make_zip(&[("../evil", Some(b"x"))], &[]),
        make_zip(&[("a/./b", Some(b"x"))], &[]),
        make_zip(&[], &[("link", "/etc/passwd")]),
    ] {
        let error = extract(bytes, &dir.path().join("y"), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::Invalid(_)), "{error}");
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = extract(
        make_zip(&[("a", Some(b"x"))], &[]),
        &dir.path().join("z"),
        &cancel,
    )
    .await
    .unwrap_err();
    assert_eq!(error, StoreError::Cancelled);
}

#[test]
fn paths_follow_node_rules() {
    assert_eq!(safe_path("a/b/").unwrap(), "a/b");
    for bad in ["", "/a", "C:x", "a\\b", "a//b", "..", "a/../b"] {
        assert!(safe_path(bad).is_err(), "{bad}");
    }
}
