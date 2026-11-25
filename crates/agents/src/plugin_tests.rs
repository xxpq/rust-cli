//! 插件 agent（agents.md §3）。

use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_base::plugin::{PluginContext, PluginOption};
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::{PluginAgentFile, parse};
use crate::context::AgentCatalog;
use crate::definitions::AgentSource;

const ROOT: &str = "/Users/dev/plugins/kit";

fn kit() -> Arc<PluginContext> {
    Arc::new(PluginContext {
        id: "kit@inline".to_owned(),
        name: "kit".to_owned(),
        root: ROOT.to_owned(),
        data: Some("/Users/dev/.zcode/cli/plugins/data/kit@inline".to_owned()),
        options: BTreeMap::from([
            ("color".to_owned(), PluginOption::Plain("blue".to_owned())),
            (
                "token".to_owned(),
                PluginOption::Secret(RedactedString::new("s3cret")),
            ),
        ]),
        declares_options: true,
    })
}

fn file(path: &str, namespace: &[&str]) -> PluginAgentFile {
    PluginAgentFile {
        plugin: kit(),
        path: format!("{ROOT}/{path}"),
        namespace: namespace.iter().map(|part| (*part).to_owned()).collect(),
    }
}

#[test]
fn names_descriptions_and_variables_follow_the_plugin_rules() {
    let raw = "---\nname: lead\nwhen_to_use: Leads the team.\ntools: Read\npermissionMode: bypassPermissions\nmaxTurns: 3\n---\nRun ${CLAUDE_PLUGIN_ROOT}/bin/x in ${ZCODE_PROJECT_DIR}.\n";
    let agent = parse(
        raw,
        &file("agents/team/lead.md", &["team"]),
        "/Users/dev/repo",
    );
    assert_eq!(agent.agent_type, "kit:team:lead");
    assert_eq!(agent.when_to_use, "Leads the team.");
    assert_eq!(agent.source, AgentSource::Plugin);
    assert_eq!(agent.tools, Some(vec!["Read".to_owned()]));
    // permissionMode 对插件 agent 无效（会越过安装时的授权）。
    assert_eq!(agent.permission_mode, None);
    assert_eq!(agent.max_turns, Some(3));
    assert_eq!(
        agent.prompt,
        "Run /Users/dev/plugins/kit/bin/x in /Users/dev/repo."
    );
    // 没有 frontmatter：名字取文件名，描述用缺省句。
    let bare = parse(
        "Just do it.",
        &file("agents/helper.md", &[]),
        "/Users/dev/repo",
    );
    assert_eq!(bare.agent_type, "kit:helper");
    assert_eq!(bare.when_to_use, "Agent from kit plugin");
    assert_eq!(bare.prompt, "Just do it.");
}

/// 正文里的数据目录与配置值（ext.md §4.10，PLUG-P0）：旧代码只换插件目录与项目目录。
#[test]
fn agent_bodies_get_the_data_dir_and_plain_options() {
    let raw = "Keep notes in ${CLAUDE_PLUGIN_DATA}; paint ${user_config.color}; key ${user_config.token}.";
    let agent = parse(raw, &file("agents/notes.md", &[]), "/Users/dev/repo");
    assert_eq!(
        agent.prompt,
        "Keep notes in /Users/dev/.zcode/cli/plugins/data/kit@inline; paint blue; key [sensitive option 'token' not available in skill content]."
    );
}

#[tokio::test]
async fn plugin_agents_sit_between_built_ins_and_user_agents() {
    let facts = EnvFacts {
        cwd: EnvPath::new("/Users/dev/repo"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    };
    let env = MemoryEnvironment::new(facts)
        .with_file(
            &format!("{ROOT}/agents/review.md"),
            "---\nname: review\ndescription: Plugin review.\n---\nP.",
        )
        .with_file(
            &format!("{ROOT}/agents/explore.md"),
            "---\ndescription: Plugin explore.\n---\nE.",
        )
        .with_file(
            "/Users/dev/.zcode/agents/kit-review.md",
            "---\nname: kit:review\ndescription: User override.\n---\nU.",
        )
        .with_file(
            "/Users/dev/.zcode/agents/reviewer.md",
            "---\nname: reviewer\ndescription: User reviewer.\n---\nR.",
        );
    let env = Arc::new(env);
    let catalog = AgentCatalog::new(env.clone(), env).with_plugins(vec![
        file("agents/review.md", &[]),
        file("agents/explore.md", &[]),
        file("agents/missing.md", &[]),
    ]);
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/repo"),
    };
    let found: Vec<(String, String, AgentSource)> = catalog
        .definitions(&workspace, &[])
        .await
        .into_iter()
        .map(|agent| (agent.agent_type, agent.when_to_use, agent.source))
        .collect();
    let tail = &found[3..];
    // 用户 agent 的名字不能带 `:`（留给插件的命名空间，agents.md §3）：`kit:review` 那个文件不认，盖不掉插件的同名 agent。
    // 以前认它，用户文件盖掉了插件的 agent。
    assert_eq!(
        tail,
        [
            (
                "kit:review".to_owned(),
                "Plugin review.".to_owned(),
                AgentSource::Plugin
            ),
            (
                "kit:explore".to_owned(),
                "Plugin explore.".to_owned(),
                AgentSource::Plugin
            ),
            (
                "reviewer".to_owned(),
                "User reviewer.".to_owned(),
                AgentSource::User
            ),
        ]
    );
}
