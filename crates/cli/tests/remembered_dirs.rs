//! `/add-dir` 记住的目录（docs/specs/rust-cli/permission-updates.md §6.2），真实进程：本机项目规则集的 `additionalDirectories`
//! 里有一个目录时，`zcode -p` 起始的会话把它当附加工作目录（接在 `--add-dir` 之后）：第一个请求的环境段列出它。修前入口不读它。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录与库、起本地 HTTP 夹具"
)]

mod sse;

use std::process::Stdio;

use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::store::LocalSettings;

#[tokio::test]
async fn remembered_directories_start_every_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work, extra, flag) = (
        base.join("home"),
        base.join("work"),
        base.join("extra"),
        base.join("flag"),
    );
    for path in [&home, &extra, &flag] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::create_dir_all(work.join(".git")).unwrap();
    // 像 TUI 的「记住」那样写进本机项目规则集（同一个存储位置）。
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), &home, &work);
    let store = rust_cli_node_store::NodeStore::new(
        paths,
        std::sync::Arc::new(rust_cli_node_store::NodeIds::new(std::sync::Arc::new(
            rust_cli_base::clock::SystemClock::utc(),
        ))),
    );
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(work.to_string_lossy()),
    };
    store
        .remember_directories(&workspace, vec![EnvPath::new(extra.to_string_lossy())])
        .await
        .unwrap();
    drop(store);
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "hello", "--add-dir", &flag.to_string_lossy()])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = fixture.main_requests().remove(0).to_string();
    let listed = format!(
        "Additional working directories:\\n  - {}\\n  - {}",
        flag.display(),
        extra.display()
    );
    assert!(request.contains(&listed), "{request}");
}
