//! 对服务器请求的回答（ext.md §3.3、§3.11 的 G12）与 `roots/list` 的 URI。

use super::*;

/// 工作线 F57：`roots/list` 的 URI 同 `url.pathToFileURL(root).href`。POSIX 的期望值是用 Bun 1.3.11
/// 与 Node 24 实测的，两者逐字相同；Windows 的是 Node 24 的 `pathToFileURL(p, {windows: true})`（`C:\`
/// 取 Windows 上的结果 `file:///C:/`）。以前不编码、UNC 写成 `file:////server/share`。
#[test]
fn root_uris_are_node_file_urls() {
    for (path, expected) in [
        ("/Users/dev/project", "file:///Users/dev/project"),
        ("/a b/c", "file:///a%20b/c"),
        ("/x#y?z%w", "file:///x%23y%3Fz%25w"),
        ("/\u{fc}/\u{e9}", "file:///%C3%BC/%C3%A9"),
        ("/a\\b", "file:///a%5Cb"),
        ("/a\tb", "file:///a%09b"),
        (
            "/a\"<>`{}|^[]@!$&'()*+,;=:~",
            "file:///a%22%3C%3E%60%7B%7D%7C%5E%5B%5D@!$&'()*+,;=:%7E",
        ),
        ("/a\u{7f}-._", "file:///a%7F-._"),
        (r"C:\Work", "file:///C:/Work"),
        (r"C:\a b\c", "file:///C:/a%20b/c"),
        (r"c:\x#y?z%w", "file:///c:/x%23y%3Fz%25w"),
        (r"C:\", "file:///C:/"),
        (r"C:\a/b", "file:///C:/a/b"),
        (r"\\server\share\repo", "file://server/share/repo"),
        (r"\\SERVER\Share\a b", "file://server/Share/a%20b"),
        (r"\\?\UNC\server\share\x", "file://server/share/x"),
        (r"\\?\C:\x", "file:///C:/x"),
    ] {
        assert_eq!(file_uri(path), expected, "{path}");
    }
}

/// 按 href 去重（工作区根也出现在 `additionalDirectories` 里时只给一次）。
#[test]
fn root_uris_are_listed_once() {
    let roots = vec!["/w".to_owned(), "/x".to_owned(), "/w".to_owned()];
    assert_eq!(
        answer("roots/list", &roots).ok(),
        Some(json!({"roots": [{"uri": "file:///w"}, {"uri": "file:///x"}]}))
    );
}

/// 其余请求一律 `-32601`，文字带方法名（同以前；rmcp 缺省的回调对 elicitation 回 `decline`）。
#[test]
fn other_requests_get_method_not_found_with_the_method() {
    for method in ["elicitation/create", "sampling/createMessage", "tasks/get"] {
        let refused = answer(method, &[]).err();
        assert_eq!(
            refused.map(|error| (error.code, error.message.into_owned())),
            Some((
                ErrorCode::METHOD_NOT_FOUND,
                format!("Method not found: {method}")
            ))
        );
    }
}

/// 声明的能力只有 `{roots: {}}`，`clientInfo` 是本工程的名字与版本，`initialize` 发给定的修订（ext.md §3.3）。
#[test]
fn the_client_declares_roots_only() {
    let answers = Answers::new(
        Vec::new(),
        &ProtocolVersion::V_2025_11_25,
        "1.2.3",
        (None, None),
    );
    assert_eq!(
        serde_json::to_value(answers.get_info()).ok(),
        Some(json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"roots": {}},
            "clientInfo": {"name": rust_cli_base::names::CLIENT_NAME, "version": "1.2.3"},
        }))
    );
}
