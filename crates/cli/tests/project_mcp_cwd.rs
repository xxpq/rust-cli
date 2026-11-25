//! 项目级 stdio MCP 服务器的 `cwd` 按声明它的那一层的目录解析（主会话审定 2026-11-05，docs/specs/rust-cli/config.md §2.7）：真实进程
//! `zcode-rs -p`，临时 home、临时仓库、本地模型夹具、假凭据。工作区是仓库的子目录 `app`，声明在仓库根的配置里；标记服务器记下
//! 自己起在哪个目录（`tests/fixtures/mcp-counter.mjs`：`start <名字> <pid> <cwd>`）。
//!
//! 缺陷修复：以前 rust 执行时相对的 `cwd` 一律按工作区根解析，而信任记录的 digest 与 Node 按声明那一层的目录解析：用户信任的是一个
//! 目录，实际跑的是另一个（18-mcp-trust.md §12.11）。
//!
//! 回落 `.agents/mcp.json`（18-mcp-trust.md §14）：`-p` 与 App、TUI 同一条规则，项目配置一个条目都没写时读它，相对的 `cwd` 按工作区根；
//! 项目配置写了服务器时不读。用户级的回落 `~/.agents/mcp.json`（§14.10）同一条规则：用户级配置没有 MCP 条目时读它。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、找 node、起真实进程与本地夹具、检查写出的文件"
)]

mod sse;
mod system_env;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use rust_cli_base::models::GLM_5_3;
use serde_json::{Value, json};
use system_env::KeepSystem as _;

/// 两个标记服务器：仓库根的 `zcode.json` 里写了相对的 `cwd`；仓库根的 `.zcode/config.json` 里没写 `cwd`。
const RELATIVE: &str = "relative";
const DEFAULT: &str = "default";
/// 回落文件里的两个标记服务器：写了相对的 `cwd`；不合法（`timeoutMs: 0`）的邻项。
const AGENTS_TOOL: &str = "agents-tool";
const AGENTS_BROKEN: &str = "agents-broken";
/// 用户级回落文件里的标记服务器、用户级配置里的标记服务器。
const USER_AGENTS: &str = "user-agents";
const USER_CONFIG: &str = "user-config";
/// `relative` 写的目录（相对声明它的那一层）。
const TOOLS: &str = "tools";
/// 工作区（进程 cwd）是仓库的这个子目录。
const APP: &str = "app";

/// 测试进程的 PATH 里的 node（`pnpm test:rust-cli` 由 node 启动，PATH 里一定有）。
fn node() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
        .expect("node is not on PATH")
}

fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value.to_string()).unwrap();
}

/// 标记日志里 `name` 起过的目录（每起一个进程一项）。
fn started_in(log: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, ' ');
            (parts.next() == Some("start") && parts.next() == Some(name))
                .then(|| parts.nth(1).unwrap_or_default().to_owned())
        })
        .collect()
}

/// `zcode-rs -p hi`，工作目录 `workspace`，临时 home，本地模型夹具；断言进程正常退出。`-p` 发第一个请求之前等 MCP 服务器连完
/// （ext.md §3.3.1）：返回时服务器都已记下起在哪里。
fn run_print(workspace: &Path, home: &Path) {
    let fixture = sse::serve(Vec::new());
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "hi"])
        .current_dir(workspace)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_agents_file_starts_in_print_mode_only_when_the_project_declares_no_servers() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, repo, log) = (base.join("home"), base.join("repo"), base.join("mcp.log"));
    for dir in [&home, &repo.join(".git"), &repo.join(TOOLS)] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-counter.mjs");
    let server =
        |name: &str| json!({"type": "stdio", "command": node(), "args": [script, log, name]});
    let mut tool = server(AGENTS_TOOL);
    tool["cwd"] = json!(TOOLS);
    let mut broken = server(AGENTS_BROKEN);
    broken["timeoutMs"] = json!(0);
    write(
        &repo.join(".agents/mcp.json"),
        &json!({"mcpServers": {AGENTS_TOOL: tool, AGENTS_BROKEN: broken}}),
    );
    run_print(&repo, &home);
    let path = |dir: &Path| dir.to_string_lossy().into_owned();
    // 相对的 `cwd` 按工作区根，不是 `.agents/`；不合法的邻项不起，也不拖垮好的
    assert_eq!(started_in(&log, AGENTS_TOOL), [path(&repo.join(TOOLS))]);
    assert!(started_in(&log, AGENTS_BROKEN).is_empty());
    // 项目配置写了服务器：不再读回落文件
    write(
        &repo.join("zcode.json"),
        &json!({"mcp": {"servers": {DEFAULT: server(DEFAULT)}}}),
    );
    run_print(&repo, &home);
    assert_eq!(started_in(&log, DEFAULT), [path(&repo)]);
    assert_eq!(started_in(&log, AGENTS_TOOL).len(), 1);
}

/// 用户级的回落（18-mcp-trust.md §14.10）：`-p` 也读 `~/.agents/mcp.json`，用户级、不经信任，相对的 `cwd` 由执行环境按工作区根解析；
/// `~/.zcode/cli/config.json` 写了服务器之后不再读它（两个文件不合并）。以前 rust 不读它。
#[test]
fn the_user_agents_file_starts_in_print_mode_until_the_user_config_declares_servers() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, repo, log) = (base.join("home"), base.join("repo"), base.join("mcp.log"));
    for dir in [&home, &repo.join(".git"), &repo.join(TOOLS)] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-counter.mjs");
    let server =
        |name: &str| json!({"type": "stdio", "command": node(), "args": [script, log, name]});
    let mut agents = server(USER_AGENTS);
    agents["cwd"] = json!(TOOLS);
    write(
        &home.join(".agents/mcp.json"),
        &json!({"mcpServers": {USER_AGENTS: agents}}),
    );
    run_print(&repo, &home);
    let path = |dir: &Path| dir.to_string_lossy().into_owned();
    assert_eq!(started_in(&log, USER_AGENTS), [path(&repo.join(TOOLS))]);
    write(
        &home.join(".zcode/cli/config.json"),
        &json!({"mcp": {"servers": {USER_CONFIG: server(USER_CONFIG)}}}),
    );
    run_print(&repo, &home);
    assert_eq!(started_in(&log, USER_CONFIG), [path(&repo)]);
    assert_eq!(started_in(&log, USER_AGENTS).len(), 1);
}

#[test]
fn a_relative_cwd_is_the_directory_of_the_layer_that_declares_it() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, repo, log) = (base.join("home"), base.join("repo"), base.join("mcp.log"));
    let workspace = repo.join(APP);
    for dir in [&home, &workspace, &repo.join(".git"), &repo.join(TOOLS)] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-counter.mjs");
    let server =
        |name: &str| json!({"type": "stdio", "command": node(), "args": [script, log, name]});
    let mut relative = server(RELATIVE);
    relative["cwd"] = json!(TOOLS);
    write(
        &repo.join("zcode.json"),
        &json!({"mcp": {"servers": {RELATIVE: relative}}}),
    );
    write(
        &repo.join(".zcode/config.json"),
        &json!({"mcp": {"servers": {DEFAULT: server(DEFAULT)}}}),
    );
    run_print(&workspace, &home);
    let path = |dir: &Path| dir.to_string_lossy().into_owned();
    assert_eq!(started_in(&log, RELATIVE), [path(&repo.join(TOOLS))]);
    // 没写 `cwd`：`.zcode/` 下的文件是 `.zcode` 的上一层，不是工作区根
    assert_eq!(started_in(&log, DEFAULT), [path(&repo)]);
}
