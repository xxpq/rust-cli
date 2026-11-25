//! `/mcp enable/disable` 写哪个文件（tui-manage.md §6.1）。
#![expect(
    clippy::disallowed_methods,
    reason = "测试准备与读取临时目录里的用户级配置"
)]

use rust_cli_kernel_api::env::{EnvFacts, ReadFile};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};

use super::*;
use rust_cli_platform::local::McpTable;

/// 回落文件（工作区根的 `.agents/mcp.json`）。
const AGENTS_FILE: &str = "/Users/dev/project/.agents/mcp.json";

/// `origins`：名字、文件、是不是用户级、文件里的服务器表。
fn files(origins: &[(&str, &str, bool, McpTable)], env: Arc<MemoryEnvironment>) -> McpConfigFiles {
    McpConfigFiles {
        origins: origins
            .iter()
            .map(|(name, path, user, table)| {
                (
                    (*name).to_owned(),
                    McpOrigin {
                        path: (*path).to_owned(),
                        user: *user,
                        table: *table,
                    },
                )
            })
            .collect(),
        files: env,
        ids: Arc::new(SequentialIds::default()),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
    }
}

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(
        MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            home: Some(EnvPath::new("/Users/dev")),
            uid: None,
        })
        .with_file(
            "/Users/dev/project/zcode.json",
            r#"{"mcp":{"servers":{"repo":{"command":"r"}}}}"#,
        )
        .with_file(AGENTS_FILE, r#"{"mcpServers":{"probe":{"command":"p"}}}"#),
    )
}

#[tokio::test]
async fn user_and_project_servers_go_to_their_own_files() {
    let home = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), home.path(), home.path());
    let user_file = home.path().join(".zcode/cli/config.json");
    std::fs::create_dir_all(user_file.parent().unwrap()).unwrap();
    std::fs::write(&user_file, r#"{"mcp":{"servers":{"docs":{"url":"u"}}}}"#).unwrap();
    let env = env();
    let files = files(
        &[
            ("docs", "user", true, McpTable::Config),
            (
                "repo",
                "/Users/dev/project/zcode.json",
                false,
                McpTable::Config,
            ),
        ],
        env.clone(),
    );
    let written = files.save(&paths, "docs", false).await.unwrap();
    assert_eq!(written, Some(user_file.display().to_string()));
    assert!(
        std::fs::read_to_string(&user_file)
            .unwrap()
            .contains("\"enabled\": false")
    );
    let written = files.save(&paths, "repo", false).await.unwrap();
    assert_eq!(written.as_deref(), Some("/Users/dev/project/zcode.json"));
    let project = env
        .read_file(
            &files.workspace,
            ReadFile {
                path: EnvPath::new("/Users/dev/project/zcode.json"),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(project.bytes).unwrap(),
        "{\n  \"mcp\": {\n    \"servers\": {\n      \"repo\": {\n        \"command\": \"r\",\n        \"enabled\": false\n      }\n    }\n  }\n}\n"
    );
    // 不在配置文件里的（`--mcp-config`、插件）：只改本次运行。
    assert_eq!(files.save(&paths, "plugin", true).await.unwrap(), None);
}

/// 定义在回落文件 `.agents/mcp.json` 里的服务器写它的 `mcpServers.<名字>.enabled`（tui-manage.md §6.1；App 设置页对这个文件也这样写）。
#[tokio::test]
async fn a_server_from_the_agents_file_is_written_in_its_own_table() {
    let home = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), home.path(), home.path());
    let env = env();
    let files = files(
        &[("probe", AGENTS_FILE, false, McpTable::Agents)],
        env.clone(),
    );
    let written = files.save(&paths, "probe", false).await.unwrap();
    assert_eq!(written.as_deref(), Some(AGENTS_FILE));
    let read = env
        .read_file(
            &files.workspace,
            ReadFile {
                path: EnvPath::new(AGENTS_FILE),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(read.bytes).unwrap(),
        "{\n  \"mcpServers\": {\n    \"probe\": {\n      \"command\": \"p\",\n      \"enabled\": false\n    }\n  }\n}\n"
    );
}

/// 定义在用户级的 `~/.agents/mcp.json` 里的服务器（18-mcp-trust.md §14.10）写那个文件的 `mcpServers.<名字>.enabled`，经 platform，
/// 用户级配置不动。
#[tokio::test]
async fn a_server_from_the_user_agents_file_is_written_there() {
    let home = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), home.path(), home.path());
    let file = home.path().join(".agents/mcp.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, r#"{"mcpServers":{"mine":{"command":"m"}}}"#).unwrap();
    let shown = file.display().to_string();
    let files = files(&[("mine", &shown, true, McpTable::Agents)], env());
    let written = files.save(&paths, "mine", false).await.unwrap();
    assert_eq!(written.as_deref(), Some(shown.as_str()));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "{\n  \"mcpServers\": {\n    \"mine\": {\n      \"command\": \"m\",\n      \"enabled\": false\n    }\n  }\n}\n"
    );
    assert!(!home.path().join(".zcode/cli/config.json").exists());
}
