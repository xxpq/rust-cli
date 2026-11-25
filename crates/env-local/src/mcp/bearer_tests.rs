use super::*;

#[test]
fn challenge_params_are_read_quoted_or_bare_like_the_ts_sdk() {
    let header =
        r#"Bearer error="insufficient_scope", scope="a b", resource_metadata=https://x.example/m"#;
    assert_eq!(challenge_param(header, "error"), Some("insufficient_scope"));
    assert_eq!(challenge_param(header, "scope"), Some("a b"));
    assert_eq!(
        challenge_param(header, "resource_metadata"),
        Some("https://x.example/m")
    );
    assert_eq!(challenge_param(header, "realm"), None);
    // 方案不是 Bearer：不读
    assert_eq!(challenge_param(r#"Basic realm="x""#, "realm"), None);
    // 参数名要在边界上：`myscope` 不是 `scope`
    assert_eq!(challenge_param(r#"Bearer myscope="x""#, "scope"), None);
    // 空值当没有
    assert_eq!(challenge_param(r#"Bearer scope="""#, "scope"), None);
}
