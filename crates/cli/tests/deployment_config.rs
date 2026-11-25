//! 部署配置的错误路径（docs/specs/rust-cli/deployment.md §6）：真实的 `zcode -p`。写错的配置报出变量与原因、退出码 1，
//! 不按缺省（本机）悄悄运行；`store` 写错或连接串的变量没设时同样报错，不落到 SQLite 上。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：写临时的部署配置、拉起 zcode 进程"
)]

use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_zcode-rs");

fn run_with(config: &str) -> Output {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("deployment.json");
    std::fs::write(&file, config).unwrap();
    Command::new(BIN)
        .args(["-p", "hi"])
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home.path())
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL_BASE_URL", "http://127.0.0.1:9")
        .env("ZCODE_RUNTIME_ENV", "test")
        .env("ZCODE_DEPLOYMENT", &file)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_bad_key_or_an_unusable_store_is_an_error_not_the_local_default() {
    for (config, reason) in [
        (r#"{"primary": "local"}"#, "unknown field"),
        (
            r#"{"store": {"postgres": {"url_env": "ZCODE_PG_URL"}}}"#,
            "unknown variant",
        ),
        (
            r#"{"store": {"pg": {"url_env": "ZCODE_PG_URL"}}}"#,
            "missing field `user_id`",
        ),
    ] {
        let output = run_with(config);
        assert_eq!(output.status.code(), Some(1), "{config}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("ZCODE_DEPLOYMENT="), "{stderr}");
        assert!(stderr.contains(reason), "{stderr}");
    }
    let unset = r#"{"store": {"pg": {"url_env": "ZCODE_PG_URL", "user_id": "alice"}}}"#;
    let output = run_with(unset);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // 带 `cloud` 的构建读连接串的变量；缺省构建没有共用的会话库，说明原因。两种都不回退本机库。
    #[cfg(feature = "cloud")]
    assert!(stderr.contains("ZCODE_PG_URL is not set"), "{stderr}");
    #[cfg(not(feature = "cloud"))]
    assert!(stderr.contains("`cloud` feature"), "{stderr}");
}
