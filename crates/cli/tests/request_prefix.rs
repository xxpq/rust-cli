//! 主线程请求的已定前缀的真实进程（docs/specs/rust-cli/request-prefix.md §6）：调试构建里每个拼了前缀的请求都
//! 再整段编码一次逐字节比，不同就 panic（进程失败）。这里跑几种会改前缀的情形——多步工具（长输出、图片）、续写（新进程）、
//! fork、自动压缩——每一次都要成功，日志里拼了前缀的请求数与预期相同。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读日志文件"
)]

mod sse;

use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::Arc;

use rust_cli_base::clock::SystemClock;
use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_node_store::{NodeIds, NodeStore};
use rust_cli_paths::{StoragePaths, StorageSettings};
use serde_json::{Value, json};

/// 一个 1×1 的 PNG。
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn bash(id: &str, command: &str) -> String {
    let input = json!({"command": command, "description": "run"}).to_string();
    sse::tool_block(0, id, "Bash", &input)
}

fn read(id: &str, path: &Path) -> String {
    let input = json!({"file_path": path}).to_string();
    sse::tool_block(0, id, "Read", &input)
}

fn run(home: &Path, work: &Path, fixture: &sse::Fixture, args: &[&str]) -> Output {
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(args)
        .args(["--permission-mode", "bypassPermissions"])
        .current_dir(work)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
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
    output
}

/// 日志里 `request.prefix` 的行：每个主线程请求一行，`context.spliced` 是拼上的前缀条数。
fn prefix_lines(home: &Path) -> Vec<Value> {
    let dir = home.join(".zcode/rust-cli/log");
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        lines.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|line| line["event"] == "request.prefix"),
        );
    }
    lines
}

fn spliced(lines: &[Value]) -> usize {
    lines
        .iter()
        .filter(|line| line["context"]["spliced"].as_u64().unwrap_or(0) > 0)
        .count()
}

async fn newest_session(home: &Path, work: &Path) -> String {
    let paths = StoragePaths::resolve(&StorageSettings::default(), home, work);
    let store = NodeStore::new(paths, Arc::new(NodeIds::new(Arc::new(SystemClock::utc()))));
    let listed = store
        .list(rust_cli_kernel_api::store::SessionFilter {
            workspace: rust_cli_kernel_api::env::WorkspaceRef {
                environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
                root: rust_cli_kernel_api::env::EnvPath::new(work.to_string_lossy()),
            },
            limit: 1,
        })
        .await
        .unwrap();
    listed[0].id.as_str().to_owned()
}

#[tokio::test]
async fn spliced_requests_equal_whole_requests_across_tools_resume_compaction_and_fork() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        work.join("notes.txt"),
        "line one\nline \"two\"\n\ttabbed 中文\n",
    )
    .unwrap();
    std::fs::write(work.join("pixel.png"), PIXEL).unwrap();

    // 一个回合四步：长输出的 Bash、读文本、读图片、回答。第 1 个请求还没有 assistant（没有切点），第 2 个整段、定下切点，
    // 第 3、4 个拼上前缀。
    let fixture = sse::serve(vec![
        sse::tools_reply(&[bash(
            "toolu_long",
            "yes 'a long line of output' | head -n 4000",
        )]),
        sse::tools_reply(&[read("toolu_text", &work.join("notes.txt"))]),
        sse::tools_reply(&[read("toolu_image", &work.join("pixel.png"))]),
        sse::text_reply("one"),
    ]);
    run(&home, &work, &fixture, &["-p", "go"]);
    assert_eq!(fixture.main_requests().len(), 4);
    let first = spliced(&prefix_lines(&home));
    assert_eq!(first, 2);
    let session = newest_session(&home, &work).await;

    // 续写（新进程）：第一个请求整段（表只在内存里），第二个拼上前缀。
    let fixture = sse::serve(vec![
        sse::tools_reply(&[bash("toolu_echo", "echo hi")]),
        sse::text_reply("two"),
    ]);
    run(
        &home,
        &work,
        &fixture,
        &["-p", "--resume", &session, "second"],
    );
    let second = spliced(&prefix_lines(&home));
    assert_eq!(second, first + 1);

    // fork：新的会话，第一个请求整段，第二个拼上。
    let fixture = sse::serve(vec![
        sse::tools_reply(&[bash("toolu_fork", "echo fork")]),
        sse::text_reply("forked"),
    ]);
    run(
        &home,
        &work,
        &fixture,
        &["-p", "--resume", &session, "--fork-session", "forked"],
    );
    let forked = spliced(&prefix_lines(&home));
    assert_eq!(forked, second + 1);

    // 自动压缩（在原来的会话上）：报很大的用量，下一步之前压缩；压缩之后整段，再下一步拼上前缀。fork 排在压缩之前：压缩之后
    // fork 在 -p 里报“preserved message … has no stored row”，修改前的二进制也一样（ISSUES 2026-11-13 PERF-RUST-B）。
    let fixture = sse::serve(vec![
        sse::tools_reply_with_usage(&[bash("toolu_big", "echo big")], 2_000_000),
        sse::text_reply("<summary>The user ran a few commands.</summary>"),
        sse::tools_reply(&[bash("toolu_after", "echo after")]),
        sse::text_reply("compacted"),
    ]);
    run(
        &home,
        &work,
        &fixture,
        &["-p", "--resume", &session, "third"],
    );
    assert_eq!(fixture.main_requests().len(), 4);
    let compacted = spliced(&prefix_lines(&home));
    // 第一个请求整段（新进程）；它的回复报了很大的用量，下一个请求之前先压缩（摘要请求不用前缀）；压缩后的第一个整段
    // （活动历史从新的边界开始），最后一个拼上。
    assert_eq!(compacted, forked + 1);
}
