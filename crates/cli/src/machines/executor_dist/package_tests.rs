//! npm 平台包的纯计算部分（remote-exec.md §26.4）：integrity 的几种写法、tar 的 ustar 前缀、pax 的 `path` 与 GNU 长名、截断与缺项。

use rust_cli_ssh_connect::Platform;

use super::super::fake_npm_tests::{build_json, gzip, header, image, sha256_hex, sri, tar};
use super::{extract, integrity_matches};

#[test]
fn integrity_needs_a_matching_sha512_entry() {
    let bytes = b"tarball bytes";
    assert!(integrity_matches(bytes, &sri(bytes)));
    assert!(integrity_matches(
        bytes,
        &format!("sha1-AAAA {}?opt", sri(bytes))
    ));
    assert!(!integrity_matches(bytes, &sri(b"other")));
    assert!(!integrity_matches(bytes, "sha1-AAAA"));
    assert!(!integrity_matches(bytes, ""));
}

#[test]
fn entries_named_by_a_ustar_prefix_or_a_pax_path_are_found() {
    let binary = image(Platform::LinuxAarch64);
    let build = build_json("0.2.0", "aarch64-unknown-linux-gnu", &sha256_hex(&binary));
    // pax 记录的长度是整条记录的字节数（含长度数字、空格与换行）。以前的夹具写成 30（实际 29）：手写的读法按行读、不看长度，
    // 库按长度读，长度不对的记录不认（真实的 tar 写出的长度都对）。
    let pax = b"29 path=package/bin/zcode-rs\n".to_vec();
    let archive = gzip(&tar(&[
        (header("build.json", "package", build.len(), b'0'), build),
        (header("PaxHeader/zcode-rs", "", pax.len(), b'x'), pax),
        (
            header("ignored-short-name", "", binary.len(), b'0'),
            binary.clone(),
        ),
    ]));
    assert_eq!(
        extract(&archive, "0.2.0", Platform::LinuxAarch64).unwrap(),
        binary
    );
}

#[test]
fn a_cut_or_incomplete_package_is_refused() {
    let binary = image(Platform::LinuxX86_64);
    let build = build_json("0.2.0", "x86_64-unknown-linux-gnu", &sha256_hex(&binary));
    let whole = tar(&[
        (
            header("package/build.json", "", build.len(), b'0'),
            build.clone(),
        ),
        (
            header("package/bin/zcode-rs", "", binary.len(), b'0'),
            binary,
        ),
    ]);
    let cut = gzip(&whole[..1500]);
    let error = extract(&cut, "0.2.0", Platform::LinuxX86_64).unwrap_err();
    assert!(error.starts_with("cannot read the package"), "{error}");
    // 截在最后一项的内容里：库读到结尾只是少给，由这一层认出截断。
    let cut = gzip(&whole[..whole.len() - 2048]);
    let error = extract(&cut, "0.2.0", Platform::LinuxX86_64).unwrap_err();
    assert_eq!(
        error,
        "cannot read the package: package/bin/zcode-rs is cut short"
    );
    let only_build = gzip(&tar(&[(
        header("package/build.json", "", build.len(), b'0'),
        build,
    )]));
    assert_eq!(
        extract(&only_build, "0.2.0", Platform::LinuxX86_64).unwrap_err(),
        "the package has no bin/zcode-rs"
    );
    assert!(
        extract(b"not gzip", "0.2.0", Platform::LinuxX86_64)
            .unwrap_err()
            .starts_with("cannot read the package")
    );
}

/// GNU 的长名（`L` 项）也认：名字在前一项的内容里。
#[test]
fn entries_named_by_a_gnu_long_name_are_found() {
    let binary = image(Platform::LinuxX86_64);
    let build = build_json("0.2.0", "x86_64-unknown-linux-gnu", &sha256_hex(&binary));
    let long = b"package/bin/zcode-rs\0".to_vec();
    let archive = gzip(&tar(&[
        (header("package/build.json", "", build.len(), b'0'), build),
        (header("././@LongLink", "", long.len(), b'L'), long),
        (header("short", "", binary.len(), b'0'), binary.clone()),
    ]));
    assert_eq!(
        extract(&archive, "0.2.0", Platform::LinuxX86_64).unwrap(),
        binary
    );
}
