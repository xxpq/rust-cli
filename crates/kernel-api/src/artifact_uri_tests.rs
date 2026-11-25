use super::*;

/// 写法照 Node（`NodeToolArtifactStore`）：两段各自 `encodeURIComponent`。
#[test]
fn a_uri_encodes_both_segments_like_node() {
    assert_eq!(
        uri(&SessionId::new("ses_1"), &ArtifactId::new("tool-result-1")),
        "zcode-artifact://ses_1/tool-result-1"
    );
    assert_eq!(
        uri(&SessionId::new("a b/c"), &ArtifactId::new("x?y")),
        "zcode-artifact://a%20b%2Fc/x%3Fy"
    );
}

/// 读法照 Node `parseArtifactUri`：两段解码、都不能为空；不是这个 scheme 或解不出时为 None。
#[test]
fn a_uri_parses_back_into_the_session_and_the_artifact() {
    let (session, id) = parse("zcode-artifact://a%20b%2Fc/x%3Fy").unwrap();
    assert_eq!((session.as_str(), id.as_str()), ("a b/c", "x?y"));
    let (session, id) = parse("zcode-artifact://ses_1//tool-result-1").unwrap();
    assert_eq!((session.as_str(), id.as_str()), ("ses_1", "tool-result-1"));
    for bad in [
        "/Users/dev/shot.png",
        "https://example.com/shot.png",
        "zcode-artifact://ses_1",
        "zcode-artifact://ses_1/",
        "zcode-artifact:///tool-result-1",
        "zcode-artifact://ses_1/%zz",
    ] {
        assert_eq!(parse(bad), None, "{bad}");
    }
}

/// 永久删除的引用扫描（02 §2.6，照 Node 的正则）：JSON 里的引号、反斜杠、空白结束一段，artifact 另到 `/`、`?`、`#` 为止；两段都解码；解不出的跳过。
#[test]
fn references_are_found_in_stored_json_like_node() {
    let text = r#"{"url":"zcode-artifact://arm-beta/shared"} zcode-artifact://a%20b/x%3Fy?raw zcode-artifact://only zcode-artifact://bad/%ZZ zcode-artifact://s/p/q"#;
    let found: Vec<(String, String)> = references(text)
        .into_iter()
        .map(|(session, artifact)| (session.as_str().to_owned(), artifact))
        .collect();
    assert_eq!(
        found,
        [
            ("arm-beta".to_owned(), "shared".to_owned()),
            ("a b".to_owned(), "x?y".to_owned()),
            ("s".to_owned(), "p".to_owned()),
        ]
    );
}
