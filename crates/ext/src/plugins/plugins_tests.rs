//! 插件目录、manifest 与组件（ext.md §4.3、§4.5、§4.9、§4.10）。

use std::collections::BTreeMap;

use rust_cli_base::plugin::PluginOption;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;
use crate::test_support::{facts, workspace};

const ROOT: &str = "/Users/dev/plugins/kit";
const DATA_ROOT: &str = "/Users/dev/.zcode/cli/plugins/data";

fn env(files: &[(&str, &str)]) -> MemoryEnvironment {
    files
        .iter()
        .fold(MemoryEnvironment::new(facts()), |env, (path, text)| {
            env.with_file(&format!("{ROOT}/{path}"), text)
        })
}

async fn session(env: &MemoryEnvironment, sources: &PluginSources) -> PluginSet {
    load(
        env,
        &workspace(),
        &[EnvPath::new(ROOT)],
        (sources, &Unhosted::default()),
    )
    .await
}

fn server<'a>(plugin: &'a Plugin, name: &str) -> &'a McpServer {
    plugin
        .mcp_servers
        .iter()
        .find(|server| server.name == name)
        .unwrap_or_else(|| panic!("no server {name}"))
}

/// 组件路径（§4.5）：skill 追加；agent 写了 manifest 就替代缺省目录（以前缺省目录与 manifest 的都加，旧代码这里多出
/// `team/lead.md`）；MCP 同名时 manifest 的覆盖 `.mcp.json` 的（以前两份都加）。
#[tokio::test]
async fn components_follow_the_path_rules() {
    let env = env(&[
        (
            ".claude-plugin/plugin.json",
            r#"{"name": "toolkit", "skills": "./extra", "agents": ["./agents/reviewer.md", "./more.md"], "hooks": {"Stop": []}, "mcpServers": {"db": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/db", "args": ["--root", "${ZCODE_PLUGIN_ROOT}"]}, "web": {"type": "http", "url": "https://manifest.example.com"}}}"#,
        ),
        ("skills/deploy/SKILL.md", "---\ndescription: Deploy\n---\n"),
        ("extra/lint/SKILL.md", "---\ndescription: Lint\n---\n"),
        ("agents/reviewer.md", "---\nname: reviewer\n---\n"),
        ("agents/team/lead.md", "Lead\n"),
        ("more.md", "More\n"),
        ("hooks/hooks.json", r#"{"hooks": {"Stop": []}}"#),
        (
            ".mcp.json",
            r#"{"mcpServers": {"web": {"type": "http", "url": "https://file.example.com"}}}"#,
        ),
    ]);
    let set = session(&env, &PluginSources::default()).await;
    let kit = &set.plugins()[0];
    assert_eq!(kit.name(), "toolkit");
    assert_eq!(kit.id(), "toolkit@inline");
    assert_eq!(
        kit.skill_dirs,
        [format!("{ROOT}/skills"), format!("{ROOT}/extra")]
    );
    let agents: Vec<&str> = kit
        .agent_files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        agents,
        [
            format!("{ROOT}/agents/reviewer.md"),
            format!("{ROOT}/more.md")
        ]
    );
    // 内联的 manifest hooks 包成文件的 `{"hooks": …}` 形状。
    assert_eq!(
        kit.hooks,
        [
            serde_json::json!({"hooks": {"Stop": []}}),
            serde_json::json!({"hooks": {"Stop": []}})
        ]
    );
    let names: Vec<&str> = kit.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["plugin:toolkit:db", "plugin:toolkit:web"]);
    let McpTransportConfig::Http { url, .. } = &server(kit, "plugin:toolkit:web").transport else {
        panic!("web is http");
    };
    assert_eq!(url, "https://manifest.example.com");
    let McpTransportConfig::Stdio { command, args, .. } =
        &server(kit, "plugin:toolkit:db").transport
    else {
        panic!("db is stdio");
    };
    assert_eq!(*command, format!("{ROOT}/bin/db"));
    assert_eq!(args, &["--root".to_owned(), ROOT.to_owned()]);
    assert!(set.errors().is_empty(), "{:?}", set.errors());
}

/// 缺省目录在、manifest 没写：照常读缺省目录；都没有、插件根有 `SKILL.md`：插件根是一个 skill（§4.5，旧代码没有）。
#[tokio::test]
async fn default_folders_and_a_root_skill() {
    let env = env(&[
        (".zcode-plugin/plugin.json", r#"{"name": "kit"}"#),
        ("SKILL.md", "---\ndescription: Root skill\n---\n"),
        ("agents/team/lead.md", "Lead\n"),
        ("commands/hello.md", "Hello\n"),
    ]);
    let set = session(&env, &PluginSources::default()).await;
    let kit = &set.plugins()[0];
    assert_eq!(kit.skill_dirs, [ROOT.to_owned()]);
    assert_eq!(kit.agent_files[0].namespace, ["team".to_owned()]);
    assert_eq!(
        kit.command_roots,
        [PluginCommandRoot::Dir(format!("{ROOT}/commands"))]
    );
}

/// 命令（§4.5.1）：manifest 写了就替代缺省目录；路径可以是 `.md` 文件；对象映射按 `source` 取文件，名字是键；没有 `source`
/// 的条目与越出插件根的路径记错误。
#[tokio::test]
async fn manifest_commands_replace_the_default_folder() {
    let env = env(&[
        (
            ".zcode-plugin/plugin.json",
            r#"{"name": "kit", "commands": {"ship": {"source": "./cmd/deploy.md", "description": "Ship it", "allowedTools": ["Bash(git:*)"]}, "inline": {"content": "x"}}}"#,
        ),
        ("commands/hello.md", "Hello\n"),
        ("cmd/deploy.md", "Deploy\n"),
    ]);
    let set = session(&env, &PluginSources::default()).await;
    let kit = &set.plugins()[0];
    assert_eq!(
        kit.command_roots,
        [PluginCommandRoot::File {
            path: format!("{ROOT}/cmd/deploy.md"),
            name: Some("ship".to_owned()),
            metadata: CommandMetadata {
                description: Some("Ship it".to_owned()),
                allowed_tools: Some(vec!["Bash(git:*)".to_owned()]),
                ..CommandMetadata::default()
            },
        }]
    );
    let kinds: Vec<(&str, PluginErrorKind)> = set
        .errors()
        .iter()
        .map(|error| (error.plugin.as_str(), error.kind))
        .collect();
    assert_eq!(kinds, [("kit@inline", PluginErrorKind::Generic)]);

    let env = self::env(&[
        (
            ".zcode-plugin/plugin.json",
            r#"{"name": "kit", "commands": ["./one.md", "../outside", "./missing"], "skills": ["../elsewhere"]}"#,
        ),
        ("one.md", "One\n"),
    ]);
    let set = session(&env, &PluginSources::default()).await;
    assert_eq!(
        set.plugins()[0].command_roots,
        [PluginCommandRoot::File {
            path: format!("{ROOT}/one.md"),
            name: None,
            metadata: CommandMetadata::default(),
        }]
    );
    let messages: Vec<&str> = set.errors().iter().map(|e| e.message.as_str()).collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    // skill 先于命令读。
    assert!(
        messages[0].contains("escapes the plugin root: ../elsewhere"),
        "{messages:?}"
    );
    assert!(
        messages[1].contains("escapes the plugin root: ../outside"),
        "{messages:?}"
    );
    assert!(
        messages[2].contains("was not found: ./missing"),
        "{messages:?}"
    );
}

fn options(pairs: &[(&str, PluginOption)]) -> BTreeMap<String, BTreeMap<String, PluginOption>> {
    BTreeMap::from([(
        "kit@inline".to_owned(),
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    )])
}

/// 数据目录与配置值（§4.9、§4.10，旧代码都没有）：stdio 服务器的环境有数据目录与插件 id；`${user_config.*}` 在 args 里只换
/// 非敏感值，敏感值只进 env；manifest 标了 `sensitive` 的明文值也当秘密；没写 `default` 的非必填项是空串。
#[tokio::test]
async fn mcp_servers_get_the_data_dir_and_options() {
    let manifest = r#"{"name": "kit", "userConfig": {
        "color": {"type": "string"},
        "token": {"type": "string", "sensitive": true},
        "legacy": {"type": "string", "sensitive": true},
        "level": {"type": "number", "default": 3},
        "note": {"type": "string"}
    }, "mcpServers": {
        "db": {"command": "db", "args": ["--color=${user_config.color}", "--level=${user_config.level}", "--note=${user_config.note}", "--data=${CLAUDE_PLUGIN_DATA}"],
               "env": {"TOKEN": "${user_config.token}", "LEGACY": "${user_config.legacy}", "ZCODE_PLUGIN_ID": "forged"}},
        "leak": {"command": "db", "args": ["${user_config.token}"]}
    }}"#;
    let env = env(&[(".zcode-plugin/plugin.json", manifest)]);
    let sources = PluginSources {
        options: options(&[
            ("color", PluginOption::Plain("blue".to_owned())),
            ("token", PluginOption::Secret(RedactedString::new("s3cret"))),
            ("legacy", PluginOption::Plain("old-plain".to_owned())),
        ]),
        data_root: Some(EnvPath::new(DATA_ROOT)),
        ..PluginSources::default()
    };
    let set = session(&env, &sources).await;
    let kit = &set.plugins()[0];
    let data = format!("{DATA_ROOT}/kit@inline");
    assert_eq!(kit.context.data.as_deref(), Some(data.as_str()));
    assert!(
        kit.context.options["legacy"].is_secret(),
        "the manifest marks it sensitive"
    );
    let names: Vec<&str> = kit.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["plugin:kit:db"], "the leaking server is skipped");
    let McpTransportConfig::Stdio { args, env, .. } = &kit.mcp_servers[0].transport else {
        panic!("db is stdio");
    };
    assert_eq!(
        args,
        &[
            "--color=blue".to_owned(),
            "--level=3".to_owned(),
            "--note=".to_owned(),
            format!("--data={data}")
        ]
    );
    assert_eq!(env["TOKEN"], "s3cret");
    assert_eq!(env["LEGACY"], "old-plain");
    assert_eq!(env["CLAUDE_PLUGIN_DATA"], data);
    assert_eq!(env["ZCODE_PLUGIN_DATA"], data);
    assert_eq!(env["ZCODE_PLUGIN_ID"], "kit@inline");
    let errors: Vec<(PluginErrorKind, &str)> = set
        .errors()
        .iter()
        .map(|error| (error.kind, error.message.as_str()))
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].0, PluginErrorKind::McpConfigInvalid);
    assert!(errors[0].1.contains("\"leak\""), "{errors:?}");
    assert!(!errors[0].1.contains("s3cret"), "{errors:?}");
}

/// 必填又没有值的配置项：这个服务器不连；manifest 没声明 `userConfig`：`${user_config.*}` 不展开（§4.10）。
#[tokio::test]
async fn required_options_without_values_skip_the_server() {
    let env = env(&[(
        ".zcode-plugin/plugin.json",
        r#"{"name": "kit", "userConfig": {"path": {"type": "string", "required": true}}, "mcpServers": {"py": {"command": "${user_config.path}"}}}"#,
    )]);
    let set = session(&env, &PluginSources::default()).await;
    assert!(set.plugins()[0].mcp_servers.is_empty());
    assert_eq!(set.errors()[0].kind, PluginErrorKind::McpConfigInvalid);
    let env = self::env(&[(
        ".zcode-plugin/plugin.json",
        r#"{"name": "kit", "mcpServers": {"py": {"command": "${user_config.path}"}}}"#,
    )]);
    let set = session(&env, &PluginSources::default()).await;
    let McpTransportConfig::Stdio { command, .. } = &set.plugins()[0].mcp_servers[0].transport
    else {
        panic!("py is stdio");
    };
    assert_eq!(command, "${user_config.path}");
}

/// 配置里的 `env` 优先（Node 先放缺省再展开配置的 `env`），插件 id 除外。
#[tokio::test]
async fn configured_env_wins_over_the_provided_variables() {
    let env = env(&[(
        ".claude-plugin/plugin.json",
        r#"{"name": "own", "mcpServers": {"db": {"command": "db", "env": {"CLAUDE_PROJECT_DIR": "/elsewhere", "TOKEN": "x"}}}}"#,
    )]);
    let set = session(&env, &PluginSources::default()).await;
    let McpTransportConfig::Stdio { env, .. } = &set.plugins()[0].mcp_servers[0].transport else {
        panic!("db is stdio");
    };
    assert_eq!(env["CLAUDE_PROJECT_DIR"], "/elsewhere");
    assert_eq!(env["TOKEN"], "x");
    assert_eq!(env["ZCODE_PLUGIN_ROOT"], ROOT);
    assert_eq!(env["ZCODE_PROJECT_DIR"], workspace().root.as_str());
}

/// `--plugin-dir`（§4.3 第 6、7 步）：`enabledPlugins["<name>@inline"]: false` 与 manifest 的 `defaultEnabled: false`
/// 都关掉它（旧代码总是加载）；关掉的会话插件不盖同名的配置目录插件。目录不存在时记 `inline[N]` 的错误。
#[tokio::test]
async fn session_plugins_can_be_disabled() {
    let other = "/Users/dev/plugins/cfg";
    let env = env(&[(
        ".zcode-plugin/plugin.json",
        r#"{"name": "kit", "defaultEnabled": false}"#,
    )])
    .with_file(
        &format!("{other}/.zcode-plugin/plugin.json"),
        r#"{"name": "kit"}"#,
    );
    let candidates = vec![PluginCandidate {
        dir: EnvPath::new(other),
        marketplace: "inline".to_owned(),
        origin: PluginOrigin::ConfigDir,
    }];
    let sources = PluginSources {
        candidates: candidates.clone(),
        ..PluginSources::default()
    };
    let dirs = [EnvPath::new(ROOT), EnvPath::new("/Users/dev/plugins/gone")];
    let unhosted = Unhosted::default();
    let set = load(&env, &workspace(), &dirs, (&sources, &unhosted)).await;
    // 会话插件关着，配置目录的同名插件照常加载。
    assert_eq!(set.plugins().len(), 1);
    assert_eq!(set.plugins()[0].dir(), other);
    assert_eq!(set.skipped()[0].reason, SkipReason::Disabled);
    let missing = &set.errors()[0];
    assert_eq!(
        (
            missing.plugin.as_str(),
            missing.kind,
            missing.path.as_deref()
        ),
        (
            "inline[1]",
            PluginErrorKind::PathNotFound,
            Some("/Users/dev/plugins/gone")
        )
    );
    // `enabledPlugins` 盖过 manifest 的缺省。
    let on = PluginSources {
        enabled_plugins: BTreeMap::from([("kit@inline".to_owned(), true)]),
        candidates,
        ..PluginSources::default()
    };
    let set = load(&env, &workspace(), &dirs[..1], (&on, &unhosted)).await;
    assert_eq!(set.plugins()[0].dir(), ROOT);
    assert_eq!(set.skipped()[0].reason, SkipReason::ShadowedBySession);
}

/// `PluginSet` 的 id 是内容哈希（§4.0）：同一份输入相同，配置值变了就变。
#[tokio::test]
async fn the_set_id_follows_the_content() {
    let env = env(&[(".zcode-plugin/plugin.json", r#"{"name": "kit"}"#)]);
    let first = session(&env, &PluginSources::default()).await;
    let again = session(&env, &PluginSources::default()).await;
    assert_eq!(first.id(), again.id());
    assert_eq!(first.id().len(), 64);
    let configured = PluginSources {
        options: options(&[("color", PluginOption::Plain("red".to_owned()))]),
        ..PluginSources::default()
    };
    assert_ne!(session(&env, &configured).await.id(), first.id());
    assert_ne!(PluginSet::default().id(), first.id());
}
