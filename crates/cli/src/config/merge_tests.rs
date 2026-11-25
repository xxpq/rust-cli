//! 三段配置的合并（config.md §2）。
use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::json;

use super::*;

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn layers<'a>(user: &'a Map<String, Value>, projects: &'a [Map<String, Value>]) -> Vec<Layer<'a>> {
    std::iter::once(Layer {
        source: RuleSource::UserSettings,
        object: user,
        path: "user",
    })
    .chain(projects.iter().map(|object| Layer {
        source: RuleSource::ProjectSettings,
        object,
        path: "project",
    }))
    .collect()
}

/// 用户级与之后的 `--settings` 层：权限、`features`、`ui` 与别的运行设置只在这两种层里（项目文件读进来时已经只剩扩展声明，
/// config.md §1.1，投影见 `files_tests.rs`）。
fn with_settings<'a>(
    user: &'a Map<String, Value>,
    later: &'a [Map<String, Value>],
) -> Vec<Layer<'a>> {
    std::iter::once(Layer {
        source: RuleSource::UserSettings,
        object: user,
        path: "user",
    })
    .chain(later.iter().map(|object| Layer {
        source: RuleSource::CliArg,
        object,
        path: "--settings",
    }))
    .collect()
}

#[test]
fn permission_keys_are_replaced_by_the_last_layer_that_sets_them() {
    let user = object(json!({"permission": {
        "mode": "edit", "allowedTools": ["Bash", "Read"], "disallowedTools": ["WebFetch"]
    }}));
    let later = [
        object(json!({"permission": {"allowedTools": ["Bash(git status:*)"]}})),
        object(json!({"permission": {"mode": "guarded", "disallowedTools": "Write"}})),
    ];
    let mut warnings = Vec::new();
    let merged = permission(&with_settings(&user, &later), &mut warnings);
    // 修复线 F20：guarded 是 guarded（以前当 bypassPermissions）。
    assert_eq!(merged.mode, Some(PermissionMode::Guarded));
    // 数组整体替换，不拼接；来源是最后写它的层。
    assert_eq!(
        merged.allowed,
        Some((RuleSource::CliArg, vec!["Bash(git status:*)".to_owned()]))
    );
    assert_eq!(
        merged.disallowed,
        Some((RuleSource::UserSettings, vec!["WebFetch".to_owned()]))
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

/// 自动模式的设置（auto-classifier.md §16.6）：`useAutoModeDuringPlan` 任一层写 false 就关、`disableAutoMode` 任一层写
/// `"disable"` 就关、`classifyAllShell` 任一层写 true 就开；四组规则各层拼接。类型不对的值记警告、跳过。
#[test]
fn auto_mode_settings_merge_like_the_reference() {
    use rust_cli_kernel_api::permission::auto_mode::{AutoModeRules, AutoModeSettings};
    let user = object(json!({"permission": {
        "useAutoModeDuringPlan": false,
        "autoMode": {"allow": ["Running make is fine"], "softDeny": ["$defaults", "Never touch prod"]}
    }}));
    let later = [object(json!({"permission": {
        "useAutoModeDuringPlan": true,
        "disableAutoMode": "disable",
        "autoMode": {"allow": ["Pushing to my fork"], "hardDeny": "no", "environment": ["Org: Example"], "classifyAllShell": true}
    }}))];
    let mut warnings = Vec::new();
    let merged = permission(&with_settings(&user, &later), &mut warnings);
    assert_eq!(
        merged.auto_mode,
        AutoModeSettings {
            disabled: true,
            use_auto_mode_during_plan: false,
            classify_all_shell: true,
            rules: AutoModeRules {
                allow: vec![
                    "Running make is fine".to_owned(),
                    "Pushing to my fork".to_owned()
                ],
                soft_deny: vec!["$defaults".to_owned(), "Never touch prod".to_owned()],
                hard_deny: Vec::new(),
                environment: vec!["Org: Example".to_owned()],
            },
        }
    );
    assert_eq!(
        warnings,
        ["--settings: permission.autoMode.hardDeny must be an array of strings"]
    );
    let mut none = Vec::new();
    assert_eq!(
        permission(&with_settings(&object(json!({})), &[]), &mut none).auto_mode,
        AutoModeSettings::default()
    );
}

#[test]
fn node_modes_map_to_command_line_modes() {
    let modes: Vec<_> = ["build", "edit", "plan", "yolo", "auto", "dontAsk"]
        .into_iter()
        .map(mode_of)
        .collect();
    assert_eq!(
        modes,
        [
            Some(PermissionMode::Default),
            Some(PermissionMode::AcceptEdits),
            Some(PermissionMode::Plan),
            Some(PermissionMode::BypassPermissions),
            Some(PermissionMode::Auto),
            None,
        ]
    );
}

#[test]
fn user_servers_shadow_project_servers_and_closer_projects_win() {
    let user = object(json!({"mcp": {"servers": {
        "shared": {"command": "user"},
        "off": {"command": "user-off", "enabled": false}
    }}}));
    let projects = [
        object(json!({"mcp": {"servers": {
            "shared": {"command": "root"}, "repo": {"command": "root"}, "off": {"command": "project"}
        }}})),
        object(json!({"mcp": {"servers": {"repo": {"command": "closer"}}}})),
    ];
    let mut warnings = Vec::new();
    let servers = mcp_servers(&layers(&user, &projects), &mut warnings);
    let found: Vec<_> = servers
        .iter()
        .map(|server| {
            let McpTransportConfig::Stdio { command, .. } = &server.transport else {
                panic!("stdio expected");
            };
            (server.name.as_str(), command.as_str(), server.scope)
        })
        .collect();
    // 用户级停用的 `off` 也遮住项目级的同名服务器。
    assert_eq!(
        found,
        [
            ("repo", "closer", McpScope::Project),
            ("shared", "user", McpScope::User),
        ]
    );
    assert!(warnings.is_empty());
}

#[test]
fn features_mcp_false_turns_mcp_off() {
    let user = object(json!({"features": {"mcp": false}}));
    assert!(!mcp_enabled(&with_settings(&user, &[])));
    let later = [object(json!({"features": {"mcp": true}}))];
    assert!(mcp_enabled(&with_settings(&user, &later)));
    assert!(mcp_enabled(&with_settings(&object(json!({})), &[])));
}

/// `ui.locale`（tui.md §7.1）：按键覆盖，只认三个值，坏值记警告、跳过这一层。
#[test]
fn the_ui_locale_is_taken_from_the_last_layer_with_a_valid_value() {
    let user = object(json!({"ui": {"locale": "zh-CN", "theme": "dark"}}));
    let later = [
        object(json!({"ui": {"locale": "en-US"}})),
        object(json!({"ui": {"locale": "fr-FR"}})),
        object(json!({"ui": "x"})),
    ];
    let mut warnings = Vec::new();
    assert_eq!(
        locale(&with_settings(&user, &later), &mut warnings),
        Some(UiLocale::EnUs)
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("fr-FR"), "{warnings:?}");
    let mut warnings = Vec::new();
    assert_eq!(
        locale(&with_settings(&object(json!({})), &[]), &mut warnings),
        None
    );
    assert!(warnings.is_empty());
}

/// 自动更新的开关（update.md §10，F48）：顶层 `checkForUpdateOnStartup` 按键覆盖，只认布尔，缺省开。
#[test]
fn the_update_check_switch_is_taken_from_the_last_layer_with_a_boolean() {
    let user = object(json!({"checkForUpdateOnStartup": false}));
    let later = [
        object(json!({"checkForUpdateOnStartup": true})),
        object(json!({"checkForUpdateOnStartup": "no"})),
    ];
    let mut warnings = Vec::new();
    assert!(check_for_update(
        &with_settings(&user, &later),
        &mut warnings
    ));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("checkForUpdateOnStartup"),
        "{warnings:?}"
    );
    let mut warnings = Vec::new();
    assert!(!check_for_update(&with_settings(&user, &[]), &mut warnings));
    let empty = object(json!({"ui": {"checkForUpdateOnStartup": false}}));
    assert!(check_for_update(&with_settings(&empty, &[]), &mut warnings));
    assert!(warnings.is_empty());
}

/// 提问框的空闲超时（tools-more.md §1.1，工作线 F72）：顶层 `askUserQuestionTimeout` 按键覆盖，只认四个值，缺省不计时。
#[test]
fn the_question_timeout_is_taken_from_the_last_layer_with_a_known_value() {
    let user = object(json!({"askUserQuestionTimeout": "5m"}));
    let later = [
        object(json!({"askUserQuestionTimeout": "60s"})),
        object(json!({"askUserQuestionTimeout": "30s"})),
    ];
    let mut warnings = Vec::new();
    assert_eq!(
        question_timeout(&with_settings(&user, &later), &mut warnings),
        Some(60_000)
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("askUserQuestionTimeout"),
        "{warnings:?}"
    );
    let mut warnings = Vec::new();
    assert_eq!(
        question_timeout(&with_settings(&user, &[]), &mut warnings),
        Some(300_000)
    );
    let never = [object(json!({"askUserQuestionTimeout": "never"}))];
    assert_eq!(
        question_timeout(&with_settings(&user, &never), &mut warnings),
        None
    );
    let ten = object(json!({"askUserQuestionTimeout": "10m"}));
    assert_eq!(
        question_timeout(&with_settings(&ten, &[]), &mut warnings),
        Some(600_000)
    );
    let empty = object(json!({}));
    assert_eq!(
        question_timeout(&with_settings(&empty, &[]), &mut warnings),
        None
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

/// 项目级 stdio 服务器的 `cwd` 按声明它的那一层的目录解析（主会话审定 2026-11-05，config.md §2.7；Node `normalizeProjectMcpServer`）：
/// 没写是那一层的目录；`.zcode/` 下的文件是 `.zcode` 的上一层；绝对路径原样。用户级的照旧相对工作区根（执行环境解析）。
#[test]
fn a_project_cwd_is_resolved_from_the_directory_of_the_declaring_layer() {
    let user = object(json!({"mcp": {"servers": {"mine": {"command": "u", "cwd": "rel"}}}}));
    let outer = object(json!({"mcp": {"servers": {
        "outer": {"command": "o", "cwd": "tools"},
        "absolute": {"command": "a", "cwd": "/opt/mcp"}
    }}}));
    let dot = object(json!({"mcp": {"servers": {"dot": {"command": "d"}}}}));
    let layers = vec![
        Layer {
            source: RuleSource::UserSettings,
            object: &user,
            path: "/Users/dev/.zcode/cli/config.json",
        },
        Layer {
            source: RuleSource::ProjectSettings,
            object: &outer,
            path: "/Users/dev/repo/zcode.json",
        },
        Layer {
            source: RuleSource::ProjectSettings,
            object: &dot,
            path: "/Users/dev/repo/.zcode/config.json",
        },
    ];
    let mut warnings = Vec::new();
    let servers = mcp_servers(&layers, &mut warnings);
    let cwd = |name: &str| {
        let server = servers.iter().find(|server| server.name == name).unwrap();
        let McpTransportConfig::Stdio { cwd, .. } = &server.transport else {
            panic!("stdio expected");
        };
        cwd.clone()
    };
    assert_eq!(cwd("outer").as_deref(), Some("/Users/dev/repo/tools"));
    assert_eq!(cwd("absolute").as_deref(), Some("/opt/mcp"));
    assert_eq!(cwd("dot").as_deref(), Some("/Users/dev/repo"));
    assert_eq!(cwd("mine").as_deref(), Some("rel"));
}

/// 停用的服务器与来源文件（`/mcp enable/disable`，tui-manage.md §6.1）。
#[test]
fn disabled_servers_and_origins_follow_the_shadowing() {
    let user =
        object(json!({"mcp": {"servers": {"off": {"command": "user-off", "enabled": false}}}}));
    let projects = [
        object(json!({"mcp": {"servers": {"repo": {"command": "root"}, "off": {"command": "p"}}}})),
        object(json!({"mcp": {"servers": {"repo": {"command": "closer"}}}})),
    ];
    let mut warnings = Vec::new();
    let found = mcp_layers(
        &layers(&user, &projects),
        Fallbacks::default(),
        &mut warnings,
    );
    let names = |servers: &[McpServer]| servers.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&found.servers), ["repo"]);
    assert_eq!(names(&found.disabled), ["off"]);
    assert!(found.origins["off"].user);
    assert!(!found.origins["repo"].user);
    // 离工作区根最近的项目文件。
    assert_eq!(found.origins["repo"].path, layers(&user, &projects)[2].path);
}

/// 回落文件（工作区根的 `.agents/mcp.json`）。
const AGENTS_FILE: &str = "/Users/dev/repo/.agents/mcp.json";

fn agents(file: ConfigFile) -> AgentsMcp {
    AgentsMcp {
        path: AGENTS_FILE.to_owned(),
        anchor: Some("/Users/dev/repo".to_owned()),
        file,
    }
}

/// 只有工作区根的回落。
fn project_fallback(agents: &AgentsMcp) -> Fallbacks<'_> {
    Fallbacks {
        project: Some(agents),
        user: None,
    }
}

fn stdio_cwd(servers: &[McpServer], name: &str) -> Option<String> {
    let server = servers.iter().find(|server| server.name == name).unwrap();
    let McpTransportConfig::Stdio { cwd, .. } = &server.transport else {
        panic!("stdio expected");
    };
    cwd.clone()
}

/// 回落 `.agents/mcp.json`（ext.md §3.1、18-mcp-trust.md §14，Node `workspace-mcp-trust` 的 `discover`）：项目级各层一个条目都没写时
/// 才用。它的服务器是项目级，相对的 `cwd` 按工作区根解析，来源记成它的 `mcpServers` 表；用户级照样遮住同名的；坏的条目只跳过它自己。
#[test]
fn the_agents_file_is_used_only_when_the_project_declares_no_servers() {
    let fallback = agents(ConfigFile::Loaded(object(json!({"mcpServers": {
        "tool": {"command": "t", "cwd": "bin"},
        "plain": {"command": "p"},
        "mine": {"command": "agents-mine"},
        "broken": {"type": "stdio"}
    }}))));
    let user = object(json!({"mcp": {"servers": {"mine": {"command": "user-mine"}}}}));
    let run = |projects: &[Map<String, Value>]| {
        let mut warnings = Vec::new();
        let found = mcp_layers(
            &layers(&user, projects),
            project_fallback(&fallback),
            &mut warnings,
        );
        (found, warnings)
    };
    // 没写 `mcp`、空表：回落
    let (found, warnings) = run(&[
        object(json!({"permission": {"allowedTools": ["Read"]}})),
        object(json!({"mcp": {"servers": {}}})),
    ]);
    let names: Vec<_> = found.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["mine", "plain", "tool"]);
    let scope = |name: &str| found.servers.iter().find(|s| s.name == name).unwrap().scope;
    assert_eq!(scope("tool"), McpScope::Project);
    assert_eq!(scope("mine"), McpScope::User);
    assert_eq!(
        stdio_cwd(&found.servers, "tool").as_deref(),
        Some("/Users/dev/repo/bin")
    );
    assert_eq!(
        stdio_cwd(&found.servers, "plain").as_deref(),
        Some("/Users/dev/repo")
    );
    assert_eq!(
        found.origins["tool"],
        McpOrigin {
            path: AGENTS_FILE.to_owned(),
            user: false,
            table: McpTable::Agents,
        }
    );
    assert!(found.origins["mine"].user);
    assert_eq!(
        warnings,
        [format!(
            "{AGENTS_FILE}: mcpServers.broken: command: required"
        )]
    );
    // 项目配置写了条目（合法的、不合法的、表不是对象、`mcp` 不是对象）：不回落
    for project in [
        json!({"mcp": {"servers": {"own": {"command": "own"}}}}),
        json!({"mcp": {"servers": {"bad": {"type": "stdio"}}}}),
        json!({"mcp": {"servers": [1]}}),
        json!({"mcp": 5}),
    ] {
        let (found, _) = run(&[object(project.clone())]);
        let names: Vec<_> = found.servers.iter().map(|s| s.name.as_str()).collect();
        assert!(
            !names.contains(&"tool") && !names.contains(&"plain"),
            "{project}: {names:?}"
        );
    }
}

/// 回落文件读不出、表不是对象、写成配置文件的形状：没有服务器；前两种记警告。
#[test]
fn a_broken_agents_file_declares_nothing() {
    let user = Map::new();
    for (file, expected) in [
        (
            ConfigFile::Invalid {
                reason: "EOF while parsing".to_owned(),
            },
            vec![format!(
                "{AGENTS_FILE}: the file is ignored: EOF while parsing"
            )],
        ),
        (
            ConfigFile::Loaded(object(json!({"mcpServers": [{"command": "x"}]}))),
            vec![format!(
                "{AGENTS_FILE}: mcpServers must be a JSON object; ignoring all MCP servers."
            )],
        ),
        (
            ConfigFile::Loaded(object(json!({"mcp": {"servers": {"x": {"command": "x"}}}}))),
            Vec::new(),
        ),
    ] {
        let mut warnings = Vec::new();
        let fallback = agents(file);
        let found = mcp_layers(
            &layers(&user, &[]),
            project_fallback(&fallback),
            &mut warnings,
        );
        assert!(found.servers.is_empty() && found.disabled.is_empty());
        assert_eq!(warnings, expected);
    }
}

/// 用户级的回落文件（home 下）。
const USER_AGENTS_FILE: &str = "/Users/dev/.agents/mcp.json";

/// 用户级的回落 `~/.agents/mcp.json`（18-mcp-trust.md §14.10，v4 App 的规则）：用户级配置一个条目都没写时才用；服务器是用户级、相对的
/// `cwd` 不锚定；排在项目级之下：项目级（含工作区根的回落）同名的覆盖它，来源记成它的 `mcpServers` 表（用户级文件）。
#[test]
fn the_user_agents_file_is_user_level_below_the_project_level() {
    let user_agents = AgentsMcp {
        path: USER_AGENTS_FILE.to_owned(),
        anchor: None,
        file: ConfigFile::Loaded(object(json!({"mcpServers": {
            "mine": {"command": "m", "cwd": "rel"},
            "both": {"command": "user-both"},
            "both-agents": {"command": "user-both-agents"}
        }}))),
    };
    let project_agents = agents(ConfigFile::Loaded(object(
        json!({"mcpServers": {"both-agents": {"command": "project-both-agents"}}}),
    )));
    let run = |user: &Map<String, Value>, projects: &[Map<String, Value>]| {
        let fallbacks = Fallbacks {
            project: Some(&project_agents),
            user: Some(&user_agents),
        };
        let mut warnings = Vec::new();
        let found = mcp_layers(&layers(user, projects), fallbacks, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        found
    };
    let command = |found: &McpLayers, name: &str| {
        let server = found.servers.iter().find(|s| s.name == name).unwrap();
        let McpTransportConfig::Stdio { command, .. } = &server.transport else {
            panic!("stdio expected");
        };
        (command.clone(), server.scope)
    };
    // 用户级配置没有条目；项目配置声明 `both`：项目级的覆盖同名的用户级回落
    let empty = Map::new();
    let found = run(
        &empty,
        &[object(
            json!({"mcp": {"servers": {"both": {"command": "project-both"}}}}),
        )],
    );
    assert_eq!(command(&found, "mine"), ("m".to_owned(), McpScope::User));
    assert_eq!(
        command(&found, "both"),
        ("project-both".to_owned(), McpScope::Project)
    );
    // 用户级不锚定 cwd（执行环境相对工作区根解析）
    assert_eq!(stdio_cwd(&found.servers, "mine").as_deref(), Some("rel"));
    assert_eq!(
        found.origins["mine"],
        McpOrigin {
            path: USER_AGENTS_FILE.to_owned(),
            user: true,
            table: McpTable::Agents,
        }
    );
    // 项目配置没有条目：工作区根的回落同样覆盖同名的用户级回落
    let found = run(&empty, &[]);
    assert_eq!(
        command(&found, "both-agents"),
        ("project-both-agents".to_owned(), McpScope::Project)
    );
    // 用户级配置写了条目（`mcp.servers` 不是对象也算写了）：不读用户级回落
    for user in [
        object(json!({"mcp": {"servers": {"own": {"command": "own"}}}})),
        object(json!({"mcp": {"servers": []}})),
    ] {
        let mut warnings = Vec::new();
        let fallbacks = Fallbacks {
            project: None,
            user: Some(&user_agents),
        };
        let found = mcp_layers(&layers(&user, &[]), fallbacks, &mut warnings);
        assert!(found.servers.iter().all(|s| s.name != "mine"), "{user:?}");
    }
}

/// `compact.autoThresholdTokens`（compact.md §2.3）：按键覆盖，正整数才算，坏值记警告、跳过这一层；定了值的不是用户级
/// 文件时说出是哪个文件（写回用户级配置改不了它）。
#[test]
fn the_auto_compact_threshold_is_taken_from_the_last_layer_with_a_positive_count() {
    let user = object(json!({"compact": {"autoThresholdTokens": 150_000}}));
    let later = [
        object(json!({"compact": {"autoThresholdTokens": 120_000}})),
        object(json!({"compact": {"autoThresholdTokens": "soon"}})),
        object(json!({"compact": {"autoThresholdTokens": 0}})),
    ];
    let mut warnings = Vec::new();
    let (tokens, pinned) = compact_at(&with_settings(&user, &later), &mut warnings);
    assert_eq!(tokens, Some(120_000));
    assert_eq!(pinned.as_deref(), Some("--settings"));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    let mut warnings = Vec::new();
    assert_eq!(
        compact_at(&with_settings(&user, &[]), &mut warnings),
        (Some(150_000), None)
    );
    assert_eq!(
        compact_at(&with_settings(&object(json!({})), &[]), &mut warnings),
        (None, None)
    );
    assert!(warnings.is_empty());
}
