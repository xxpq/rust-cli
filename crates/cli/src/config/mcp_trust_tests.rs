//! 工作区 MCP 的信任（18-mcp-trust.md §12.2–§12.3）：digest 与 Node 对拍、准入、待信任的提示、锁里的重验。
#![allow(
    clippy::disallowed_methods,
    reason = "测试在临时 home 里摆用户级配置与写坏记录文件"
)]
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryEnvironment;
use serde_json::{Value, json};

use super::super::assemble;
use super::*;

const FIXTURE: &str = include_str!("../../tests/fixtures/workspace-mcp.json");
const GOLDEN: &str = include_str!("../../tests/fixtures/workspace-mcp-golden.json");
/// Node 一侧摆文件的根目录（`scripts/rust-cli/mcp-digest-golden.mjs` 的 `ROOT`）：digest 含绝对路径。
const GOLDEN_ROOT: &str = "/tmp/zcode-mcp-digest-golden";
/// 夹具里只在 Windows 上成立的一例。
const WIN32: &str = "win32";

fn environment(files: &[(String, String)]) -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new("/"),
        platform: "linux".to_owned(),
        os_version: "Linux".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: true,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    };
    files
        .iter()
        .fold(MemoryEnvironment::new(facts), |env, (path, text)| {
            env.with_file(path, text)
        })
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

/// 临时 home（用户级配置在 `~/.zcode/cli/config.json`）与它的存储位置。
fn home(user: Option<&Value>) -> (tempfile::TempDir, StoragePaths) {
    home_with(user, None)
}

/// 同 [`home`]，另摆 home 下的文件（夹具的 `home` 段：相对 home 的路径 → 内容，例如用户级的 `~/.agents/mcp.json`）。
fn home_with(user: Option<&Value>, files: Option<&Value>) -> (tempfile::TempDir, StoragePaths) {
    let dir = tempfile::tempdir().unwrap();
    let put = |path: std::path::PathBuf, text: String| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    };
    if let Some(user) = user {
        put(dir.path().join(".zcode/cli/config.json"), user.to_string());
    }
    for (path, content) in files.and_then(Value::as_object).into_iter().flatten() {
        put(dir.path().join(path), content.to_string());
    }
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    (dir, paths)
}

/// 进程环境（http hook 的插值用）：这些测试里是空的。
static PROCESS_ENV: std::sync::LazyLock<rust_cli_base::secret::EnvSnapshot> =
    std::sync::LazyLock::new(Default::default);

fn inputs<'a>(
    env: &'a MemoryEnvironment,
    workspace: &'a WorkspaceRef,
    paths: &'a StoragePaths,
) -> ConfigInputs<'a> {
    ConfigInputs {
        files: env,
        workspace,
        paths,
        process_env: &PROCESS_ENV,
        flag: None,
        project_is_local: true,
        shared_process: true,
    }
}

/// 夹具的一例摆进内存：Node 一侧的同一组路径（`<根>/c<序号>/…`）。
fn case_files(index: usize, case: &Value) -> (String, Vec<(String, String)>) {
    let base = format!("{GOLDEN_ROOT}/c{index}");
    let mut files: Vec<(String, String)> = case["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, content)| {
            let text = match content {
                Value::String(raw) => raw.clone(),
                other => other.to_string(),
            };
            (format!("{base}/{path}"), text)
        })
        .collect();
    if let Some(git) = case["git"].as_str() {
        files.push((format!("{base}/{git}/.git/HEAD"), "ref".to_owned()));
    }
    let root = format!("{base}/{}", case["workspace"].as_str().unwrap());
    (root, files)
}

/// 生产的组装（文件 → 合并 → 声明）；Windows 那一例按 Windows 的规则算（Node 一侧同样只换了 `process.platform`）。
async fn declared(index: usize, case: &Value) -> Vec<McpDeclaration> {
    let (root, files) = case_files(index, case);
    let env = environment(&files);
    let workspace = workspace(&root);
    let user = case.get("user").filter(|user| !user.is_null());
    let (_dir, paths) = home_with(user, case.get("home"));
    let inputs = inputs(&env, &workspace, &paths);
    if case["platform"] == WIN32 {
        return current(&inputs, true).await.unwrap_or_default();
    }
    // 组装与锁里的重读是同一个结果（同一组文件）
    let assembled = assemble(&inputs).await.mcp_trust.declarations().to_vec();
    assert_eq!(current(&inputs, false).await.unwrap_or_default(), assembled);
    assembled
}

#[tokio::test]
async fn digests_match_the_node_golden() {
    let cases: Vec<Value> = serde_json::from_str(FIXTURE).unwrap();
    let golden: Vec<Value> = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(cases.len(), golden.len());
    for (index, (case, expected)) in cases.iter().zip(&golden).enumerate() {
        let name = case["name"].as_str().unwrap();
        let mut actual = declared(index, case).await;
        actual.sort_by(|left, right| left.name.cmp(&right.name));
        let actual: Value = actual
            .iter()
            .map(|declaration| {
                (
                    declaration.name.clone(),
                    json!({"declarationDigest": declaration.digest, "configuredEnabled": declaration.enabled}),
                )
            })
            .collect::<Map<String, Value>>()
            .into();
        let expected: Value = expected["servers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(server, review)| {
                (
                    server.clone(),
                    json!({
                        "declarationDigest": review["declarationDigest"],
                        "configuredEnabled": review["configuredEnabled"],
                    }),
                )
            })
            .collect::<Map<String, Value>>()
            .into();
        assert_eq!(actual, expected, "{name}");
    }
}

const ROOT: &str = "/Users/dev/repo";
const CONFIG: &str = "/Users/dev/repo/.zcode/config.json";

fn repo(servers: &Value) -> MemoryEnvironment {
    environment(&[
        (format!("{ROOT}/.git/HEAD"), "ref".to_owned()),
        (
            CONFIG.to_owned(),
            json!({"mcp": {"servers": servers}}).to_string(),
        ),
    ])
}

async fn trust_of(env: &MemoryEnvironment, paths: &StoragePaths) -> WorkspaceMcpTrust {
    let workspace = workspace(ROOT);
    assemble(&inputs(env, &workspace, paths)).await.mcp_trust
}

const AT: &str = "2026-11-05T08:00:00.000Z";

#[tokio::test]
async fn admission_follows_the_record_of_the_assembled_declaration() {
    let (_dir, paths) = home(None);
    let env =
        repo(&json!({"evil": {"command": "evil"}, "off": {"command": "off", "enabled": false}}));
    let trust = trust_of(&env, &paths).await;
    assert!(!trust.admits("evil").await);
    // 启用的才提示
    let pending = trust.pending().await;
    assert_eq!(
        pending
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["evil"]
    );
    let digest = pending[0].1.clone();
    let workspace = workspace(ROOT);
    let decision = McpDecision {
        server: "evil",
        digest: &digest,
        trusted: true,
    };
    assert!(
        decide(&inputs(&env, &workspace, &paths), &decision, AT)
            .await
            .unwrap()
    );
    assert!(trust.admits("evil").await);
    assert!(trust.pending().await.is_empty());
    // 不是项目级声明的名字不准入
    assert!(!trust.admits("unknown").await);
    // 记录坏了：全部未信任、全部算待信任
    std::fs::write(paths.mcp_trust_store(None).as_path(), "{").unwrap();
    assert!(!trust.admits("evil").await);
    assert_eq!(trust.pending().await.len(), 1);
}

#[tokio::test]
async fn decide_rechecks_the_declaration_on_disk_under_the_lock() {
    let (_dir, paths) = home(None);
    let before = repo(&json!({"evil": {"command": "evil"}}));
    let digest = trust_of(&before, &paths).await.pending().await[0].1.clone();
    // 审核之后声明变了：拒绝，什么都不写
    let after = repo(&json!({"evil": {"command": "evil", "args": ["--more"]}}));
    let workspace = workspace(ROOT);
    let decision = |server: &'static str, trusted: bool| McpDecision {
        server,
        digest: &digest,
        trusted,
    };
    let changed = decide(
        &inputs(&after, &workspace, &paths),
        &decision("evil", true),
        AT,
    )
    .await;
    assert!(matches!(changed, Err(McpTrustError::DeclarationChanged)));
    let missing = decide(
        &inputs(&before, &workspace, &paths),
        &decision("other", true),
        AT,
    )
    .await;
    assert!(matches!(missing, Err(McpTrustError::DeclarationChanged)));
    assert!(!paths.mcp_trust_store(None).as_path().exists());
    // 项目配置读不出：没有可批准的声明
    let broken = environment(&[
        (format!("{ROOT}/.git/HEAD"), "ref".to_owned()),
        (CONFIG.to_owned(), "{".to_owned()),
    ]);
    let unreadable = decide(
        &inputs(&broken, &workspace, &paths),
        &decision("evil", true),
        AT,
    )
    .await;
    assert!(matches!(unreadable, Err(McpTrustError::ConfigUnreadable)));
    // 设值语义：已是现状时不写
    let trusted = decide(
        &inputs(&before, &workspace, &paths),
        &decision("evil", true),
        AT,
    )
    .await;
    assert!(trusted.unwrap());
    let again = decide(
        &inputs(&before, &workspace, &paths),
        &decision("evil", true),
        AT,
    )
    .await;
    assert!(!again.unwrap());
}

/// 身份与 digest 的工作区根是规范化的写法（Node `resolve`）：Host 给的路径带结尾的分隔符时，记录照样对得上。
#[tokio::test]
async fn the_workspace_root_is_normalized_like_node() {
    let (_dir, paths) = home(None);
    let env = repo(&json!({"evil": {"command": "evil"}}));
    let plain = trust_of(&env, &paths).await.pending().await;
    let trailing = workspace(&format!("{ROOT}/"));
    let slashed = assemble(&inputs(&env, &trailing, &paths))
        .await
        .mcp_trust
        .pending()
        .await;
    assert_eq!(plain, slashed);
}

/// 回落的声明（18-mcp-trust.md §14）：锁里的重读也认它（以前回 `DeclarationChanged`，设置页提示「声明已变化」）；项目配置后来写了
/// 服务器、回落不再生效时，同一个 digest 不再是声明。
#[tokio::test]
async fn decide_accepts_a_fallback_declaration_while_the_fallback_applies() {
    let (_dir, paths) = home(None);
    let agents = format!("{ROOT}/.agents/mcp.json");
    let fallback = environment(&[
        (format!("{ROOT}/.git/HEAD"), "ref".to_owned()),
        (
            agents.clone(),
            json!({"mcpServers": {"evil": {"command": "evil"}, "bad": {"type": "stdio"}}})
                .to_string(),
        ),
    ]);
    let trust = trust_of(&fallback, &paths).await;
    let pending = trust.pending().await;
    // 坏邻项不是声明，也不拖垮好的那个
    assert_eq!(
        pending
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["evil"]
    );
    assert!(!trust.admits("evil").await);
    let digest = pending[0].1.clone();
    let workspace = workspace(ROOT);
    let decision = McpDecision {
        server: "evil",
        digest: &digest,
        trusted: true,
    };
    let declared = environment(&[
        (format!("{ROOT}/.git/HEAD"), "ref".to_owned()),
        (
            agents,
            json!({"mcpServers": {"evil": {"command": "evil"}}}).to_string(),
        ),
        (
            CONFIG.to_owned(),
            json!({"mcp": {"servers": {"own": {"command": "own"}}}}).to_string(),
        ),
    ]);
    let changed = decide(&inputs(&declared, &workspace, &paths), &decision, AT).await;
    assert!(matches!(changed, Err(McpTrustError::DeclarationChanged)));
    assert!(!paths.mcp_trust_store(None).as_path().exists());
    assert!(
        decide(&inputs(&fallback, &workspace, &paths), &decision, AT)
            .await
            .unwrap()
    );
    assert!(trust.admits("evil").await);
}
