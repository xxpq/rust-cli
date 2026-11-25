use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::{definition, load};
use crate::definitions::{AgentDefinition, AgentSource};

/// 一个 agent 文件的定义（解析 frontmatter 再取字段）。
fn parse(raw: &str, source: AgentSource) -> Option<AgentDefinition> {
    definition(&crate::frontmatter::parse(raw), source)
}

fn facts(cwd: &str) -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new(cwd),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: true,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    }
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

/// 带全部键的子代理文件；`model` 是缺省的测试模型。
fn reviewer() -> String {
    format!(
        "---\nname: reviewer\ndescription: Reviews code.\\nUse after edits.\ntools: Read, Grep\nmodel: {GLM_5_3}\nmaxTurns: 4\npermissionMode: acceptEdits\nbackground: true\n---\n\nYou review code.\n"
    )
}

#[test]
fn parses_the_frontmatter_keys() {
    let agent = parse(&reviewer(), AgentSource::Project).unwrap();
    assert_eq!(agent.agent_type, "reviewer");
    assert_eq!(agent.when_to_use, "Reviews code.\nUse after edits.");
    assert_eq!(
        agent.tools,
        Some(vec!["Read".to_owned(), "Grep".to_owned()])
    );
    assert_eq!(agent.prompt, "You review code.");
    assert_eq!(agent.model.as_deref(), Some(GLM_5_3));
    assert_eq!(agent.max_turns, Some(4));
    assert_eq!(agent.permission_mode, Some(PermissionMode::AcceptEdits));
    assert!(agent.background);
    assert!(parse("---\nname: x\n---\nbody", AgentSource::User).is_none());
    assert!(parse("---\ndescription: y\n---\nbody", AgentSource::User).is_none());
}

/// `name` 与 `description` 要是非空字符串、原样（agents.md §3）：数的、以 `-` 开头的、含冒号（NFKC 之后）的、太长的名字，
/// 数的描述，文件都不认；带空白的引号值留着空白。以前按标量的文字读、去首尾空白，这些文件都认了。
#[test]
fn name_and_description_must_be_plain_strings() {
    let agent = |frontmatter: &str| {
        parse(
            &format!("---\n{frontmatter}\n---\nbody\n"),
            AgentSource::Project,
        )
    };
    for rejected in [
        "name: 123\ndescription: Numeric",
        "name: true\ndescription: Boolean",
        "name: -dash\ndescription: Dash",
        "name: team:lead\ndescription: Colon",
        "name: team\u{ff1a}lead\ndescription: Fullwidth colon",
        "name: reviewer\ndescription: 42",
        "name: reviewer\ndescription: \"\"",
    ] {
        assert!(agent(rejected).is_none(), "{rejected}");
    }
    let long = "x".repeat(257);
    assert!(agent(&format!("name: {long}\ndescription: Long")).is_none());
    assert!(agent(&format!("name: {}\ndescription: Long", &long[1..])).is_some());
    let padded = agent("name: \" padded \"\ndescription: \"  Padded.  \"").unwrap();
    assert_eq!(padded.agent_type, " padded ");
    assert_eq!(padded.when_to_use, "  Padded.  ");
}

/// `tools` 空着是空清单（子代理没有工具），空格也分隔；`model` 只认字符串（agents.md §3）。以前空着按没写（全部工具）、
/// `Read Grep` 是一个名字。
#[test]
fn an_empty_tools_key_means_no_tools() {
    let agent = |frontmatter: &str| {
        parse(
            &format!("---\nname: a\ndescription: d\n{frontmatter}\n---\nbody\n"),
            AgentSource::Project,
        )
        .unwrap()
    };
    assert_eq!(agent("tools:").tools, Some(Vec::new()));
    assert_eq!(agent("tools: \"\"").tools, Some(Vec::new()));
    assert_eq!(
        agent("tools: Read Grep").tools,
        Some(vec!["Read".to_owned(), "Grep".to_owned()])
    );
    assert_eq!(agent("tools: Read, *").tools, None);
    assert_eq!(
        agent("disallowedTools:").disallowed_tools,
        Vec::<String>::new()
    );
    assert_eq!(agent("model: 4").model, None);
    assert_eq!(
        agent("model: \" Inherit \"").model.as_deref(),
        Some("inherit")
    );
}

/// frontmatter 的 `permissionMode`：先把 `manual` 换成 `default`，再按 6 个模式校验（修复线 F58）。
/// 以前不认 `manual`；`guarded` 是 Node 的模式，这里仍不认。
#[test]
fn the_permission_mode_accepts_the_manual_alias() {
    let mode = |value: &str| {
        let raw = format!("---\nname: x\ndescription: y\npermissionMode: {value}\n---\nbody");
        parse(&raw, AgentSource::Project).unwrap().permission_mode
    };
    assert_eq!(mode("manual"), Some(PermissionMode::Default));
    assert_eq!(mode("default"), Some(PermissionMode::Default));
    assert_eq!(mode("dontAsk"), Some(PermissionMode::DontAsk));
    assert_eq!(mode("auto"), Some(PermissionMode::Auto));
    assert_eq!(mode("guarded"), None);
    assert_eq!(mode("yolo"), None);
}

#[tokio::test]
async fn loads_user_then_project_dirs_up_to_the_git_root() {
    let env = MemoryEnvironment::new(facts("/Users/dev/repo/pkg"))
        .with_file(
            "/Users/dev/.zcode/agents/user.md",
            "---\nname: helper\ndescription: User helper.\n---\nUser.",
        )
        .with_file("/Users/dev/repo/.git/HEAD", "ref: refs/heads/main\n")
        .with_file(
            "/Users/dev/repo/.zcode/agents/helper.md",
            "---\nname: helper\ndescription: Repo helper.\n---\nRepo.",
        )
        .with_file("/Users/dev/repo/pkg/.zcode/agents/reviewer.md", &reviewer())
        .with_file("/Users/dev/repo/pkg/.zcode/agents/notes.txt", "ignored")
        .with_file(
            "/Users/dev/outside/.zcode/agents/x.md",
            "---\nname: x\ndescription: X.\n---\n",
        );
    let layers = load(&env, &workspace("/Users/dev/repo/pkg"), &env.facts, &[]).await;
    let names: Vec<Vec<(&str, AgentSource)>> = layers
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|agent| (agent.when_to_use.as_str(), agent.source))
                .collect()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            vec![("User helper.", AgentSource::User)],
            vec![("Repo helper.", AgentSource::Project)],
            vec![("Reviews code.\nUse after edits.", AgentSource::Project)],
        ]
    );
}

#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_does_not_stop_the_walk() {
    // 找 git 根时只认目录与文件；FIFO 等别的种类继续往上（F58：以前任何种类都算）。
    let env = MemoryEnvironment::new(facts("/Users/dev/repo/pkg"))
        .with_special("/Users/dev/repo/pkg/.git")
        .with_file("/Users/dev/repo/.git/HEAD", "ref: refs/heads/main\n")
        .with_file(
            "/Users/dev/repo/.zcode/agents/helper.md",
            "---\nname: helper\ndescription: Repo helper.\n---\nRepo.",
        );
    let layers = load(&env, &workspace("/Users/dev/repo/pkg"), &env.facts, &[]).await;
    let found: Vec<&str> = layers
        .iter()
        .flatten()
        .map(|agent| agent.when_to_use.as_str())
        .collect();
    assert_eq!(found, ["Repo helper."]);
}

#[tokio::test]
async fn the_project_walk_stops_below_home_or_at_the_git_root() {
    use rust_cli_kernel_api::env_files::PathStyle;
    use rust_cli_kernel_api::env_git::project_levels;
    let ws = workspace("/Users/dev/a/b");
    let walk = async |env: &MemoryEnvironment| {
        project_levels(
            env,
            &ws,
            PathStyle::Posix,
            "/Users/dev/a/b",
            Some("/Users/dev"),
        )
        .await
    };
    // 不在仓库里：到 home 之前为止（home 不在内）。
    let plain = MemoryEnvironment::new(facts("/Users/dev/a/b"));
    assert_eq!(walk(&plain).await, ["/Users/dev/a/b", "/Users/dev/a"]);
    // 在仓库里：到 git 根为止（含）。
    let repo = MemoryEnvironment::new(facts("/Users/dev/a/b"))
        .with_file("/Users/dev/a/b/.git", "gitdir: x");
    assert_eq!(walk(&repo).await, ["/Users/dev/a/b"]);
}

/// 会话的附加工作目录（session-dirs.md §3.3）：它们的 `.zcode/agents` 是项目级的一层，排在工作区的项目层级之前
/// （后者覆盖：同名时工作区的生效）。
#[tokio::test]
async fn additional_directories_add_a_project_layer_before_the_workspace() {
    let env = MemoryEnvironment::new(facts("/Users/dev/repo"))
        .with_file("/Users/dev/repo/.git/HEAD", "ref: refs/heads/main\n")
        .with_file(
            "/Users/dev/repo/.zcode/agents/helper.md",
            "---\nname: helper\ndescription: Repo helper.\n---\nRepo.",
        )
        .with_file(
            "/Users/dev/other/.zcode/agents/helper.md",
            "---\nname: helper\ndescription: Other helper.\n---\nOther.",
        )
        .with_file(
            "/Users/dev/other/.zcode/agents/probe.md",
            "---\nname: probe\ndescription: Other probe.\n---\nProbe.",
        );
    let other = [EnvPath::new("/Users/dev/other")];
    let layers = load(&env, &workspace("/Users/dev/repo"), &env.facts, &other).await;
    let names: Vec<Vec<&str>> = layers
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|agent| agent.when_to_use.as_str())
                .collect()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            Vec::new(),
            vec!["Other helper.", "Other probe."],
            vec!["Repo helper."],
        ]
    );
    let merged = crate::definitions::merge(layers);
    let helper = merged
        .iter()
        .find(|agent| agent.agent_type == "helper")
        .unwrap();
    assert_eq!(helper.when_to_use, "Repo helper.");
    assert!(merged.iter().any(|agent| agent.agent_type == "probe"));
}
