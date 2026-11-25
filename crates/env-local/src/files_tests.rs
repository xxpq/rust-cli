#![expect(clippy::disallowed_methods, reason = "测试直接准备工作区夹具")]

use std::fs;

use rust_cli_kernel_api::env::EnvError;

use super::read_tail;

#[test]
fn a_large_file_gives_only_its_last_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.output");
    let mut content = vec![b'x'; 2 << 20];
    content.extend_from_slice(b"END\n");
    fs::write(&path, &content).unwrap();
    let tail = read_tail(&path, 6).unwrap();
    assert_eq!(tail.bytes, b"xxEND\n");
    assert_eq!(tail.total_bytes, content.len() as u64);
}

#[test]
fn a_small_file_is_read_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.output");
    fs::write(&path, b"a\r\nb\n").unwrap();
    let tail = read_tail(&path, 8192).unwrap();
    // 原样的字节：不解码、不去 `\r`
    assert_eq!(tail.bytes, b"a\r\nb\n");
    assert_eq!(tail.total_bytes, 5);
}

#[test]
fn a_missing_file_is_not_found_and_a_directory_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        read_tail(&dir.path().join("gone"), 8),
        Err(EnvError::NotFound { .. })
    ));
    assert!(matches!(read_tail(dir.path(), 8), Err(EnvError::Io { .. })));
}
