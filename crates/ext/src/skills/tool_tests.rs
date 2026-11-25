//! Skill 工具（ext.md §2.4）：结果形状与错误文本。

use std::sync::Arc;

use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};
use serde_json::json;

use super::*;
use crate::skills::runner;
use crate::test_support::{AllowedGate, CWD, EchoBash, facts, tool_context};

const GREET: &str = "---\ndescription: Greet someone by name\nwhen_to_use: When the user asks for a greeting\nargument-hint: <name>\narguments: [who]\nallowed-tools: Bash(echo:*)\n---\nSay hello to $ARGUMENTS ($who, first: $0).\nSkill dir: ${CLAUDE_SKILL_DIR}\nInline: !`echo inline-ok`\n";
const HELPER: &str = "---\nname: Helper\ndescription: A helper the model must not call\ndisable-model-invocation: true\n---\nHidden helper.\n";

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(
        MemoryEnvironment::new(facts())
            .with_file(&format!("{CWD}/.zcode/skills/greet/SKILL.md"), GREET)
            .with_file(&format!("{CWD}/.zcode/skills/helper/SKILL.md"), HELPER)
            .with_file(
                &format!("{CWD}/.zcode/skills/plain/SKILL.md"),
                "# Plain skill\n\nNo frontmatter here.\n",
            )
            .with_file(
                &format!("{CWD}/.zcode/skills/vars/SKILL.md"),
                "${ZCODE_PROJECT_DIR} ${CLAUDE_SESSION_ID}\n",
            ),
    )
}

fn tool(env: Arc<MemoryEnvironment>) -> SkillTool {
    let catalog = Arc::new(SkillCatalog::new(env.clone(), env));
    let ids: Arc<dyn Ids> = Arc::new(SequentialIds::default());
    let bash = Arc::new(EchoBash::default());
    SkillTool::new(
        catalog,
        ids.clone(),
        Some(runner(bash, Arc::new(AllowedGate::default()), ids)),
    )
}

fn text(output: &ToolOutput) -> &str {
    match &output.content[..] {
        [ToolResultContent::Text { text }] => text,
        other => panic!("unexpected content {other:?}"),
    }
}

fn body(call: &ToolCallOutput) -> &str {
    match &call.new_messages[..] {
        [
            Message::User(UserMessage {
                content,
                origin: UserOrigin::Meta { source },
                ..
            }),
        ] if source == SKILL_MESSAGE_SOURCE => match &content[..] {
            [ContentBlock::Text { text }] => text,
            other => panic!("unexpected content {other:?}"),
        },
        other => panic!("unexpected messages {other:?}"),
    }
}

#[tokio::test]
async fn k1_greet_loads_the_body_after_the_result() {
    let env = env();
    let tool = tool(env.clone());
    let context = tool_context(env, PermissionMode::DontAsk);
    let (progress, _ignored) = mpsc::channel(4);
    let call = tool
        .call_with_messages(json!({"skill": "greet", "args": "Ada"}), context, progress)
        .await
        .unwrap();
    assert_eq!(text(&call.output), "Launching skill: greet");
    assert_eq!(
        body(&call),
        format!(
            "Base directory for this skill: {CWD}/.zcode/skills/greet\n\nSay hello to Ada (Ada, first: Ada).\nSkill dir: {CWD}/.zcode/skills/greet\nInline: out:echo inline-ok\n"
        )
    );
}

#[tokio::test]
async fn k1_plain_and_variables() {
    let env = env();
    let tool = tool(env.clone());
    let (progress, _ignored) = mpsc::channel(4);
    let context = tool_context(env.clone(), PermissionMode::DontAsk);
    let call = tool
        .call_with_messages(
            json!({"skill": "/plain", "args": "extra words"}),
            context.clone(),
            progress.clone(),
        )
        .await
        .unwrap();
    assert_eq!(text(&call.output), "Launching skill: plain");
    assert_eq!(
        body(&call),
        format!(
            "Base directory for this skill: {CWD}/.zcode/skills/plain\n\n# Plain skill\n\nNo frontmatter here.\n\n\nARGUMENTS: extra words"
        )
    );
    let vars = tool
        .call_with_messages(json!({"skill": "vars"}), context, progress)
        .await
        .unwrap();
    assert!(
        body(&vars).ends_with(&format!("{CWD} sess_1\n")),
        "{}",
        body(&vars)
    );
}

#[tokio::test]
async fn k1_validation_errors() {
    let env = env();
    let tool = tool(env.clone());
    let context = tool_context(env, PermissionMode::DontAsk);
    let message = |validation: Validation| match validation {
        Validation::Invalid { message, .. } => message,
        Validation::Ok => panic!("expected an error"),
    };
    assert_eq!(
        message(
            tool.validate_input(&json!({"skill": "nope"}), &context)
                .await
        ),
        "Unknown skill: nope"
    );
    assert_eq!(
        message(
            tool.validate_input(&json!({"skill": "gret"}), &context)
                .await
        ),
        "Unknown skill: gret. Did you mean greet?"
    );
    assert_eq!(
        message(tool.validate_input(&json!({"skill": "  "}), &context).await),
        "Invalid skill format:   "
    );
    assert_eq!(
        message(
            tool.validate_input(&json!({"skill": "helper"}), &context)
                .await
        ),
        "Skill helper cannot be used with Skill tool due to disable-model-invocation. Ask the user to run /helper themselves — it cannot be invoked via the Skill tool. Do not replicate this skill's workflow by other means — it is reserved for explicit user invocation."
    );
    assert_eq!(
        tool.validate_input(&json!({"skill": "greet"}), &context)
            .await,
        Validation::Ok
    );
}

#[tokio::test]
async fn only_safe_skills_skip_the_permission_chain() {
    let env = env();
    let tool = tool(env.clone());
    let context = tool_context(env, PermissionMode::Default);
    let decision = |name: &'static str| {
        let tool = &tool;
        let context = &context;
        async move {
            tool.check_permissions(&json!({"skill": name}), context)
                .await
        }
    };
    assert_eq!(
        decision("plain").await,
        PermissionDecision::Allow {
            updated_input: None
        }
    );
    assert_eq!(decision("greet").await, PermissionDecision::Passthrough);
}

/// 判决主体是 skill 的名字原文（判决链去掉前导 `/`；kernel-api `PermissionSubject`，F125）。
#[test]
fn the_permission_subject_is_the_requested_skill() {
    let tool = tool(env());
    assert_eq!(
        tool.permission_subject(&json!({"skill": "/greet", "args": "Ada"})),
        Some(rust_cli_kernel_api::permission::PermissionSubject::Skill {
            name: "/greet".to_owned()
        })
    );
}

/// 插件的 skill 与命令（ext.md §4.5、§4.5.1、§4.10，PLUG-P0）：命令名字带插件前缀、排在插件 skill 之前；正文换插件目录、数据目录与
/// 非敏感的配置值（`!` 转义），敏感键换成占位句；`allowed-tools` 换插件变量。旧代码没有插件命令，正文也不换这些变量。
#[tokio::test]
async fn plugin_skills_and_commands_expand_the_plugin_variables() {
    use rust_cli_base::plugin::{PluginContext, PluginOption};
    use rust_cli_base::secret::RedactedString;
    let root = "/Users/dev/plugins/kit";
    let env = Arc::new(
        MemoryEnvironment::new(facts())
            .with_file(
                &format!("{root}/skills/notes/SKILL.md"),
                "---\ndescription: Notes\nallowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/bin/x:*)\n---\nRoot ${CLAUDE_PLUGIN_ROOT}, data ${ZCODE_PLUGIN_DATA}, color ${user_config.color}, key ${user_config.token}.\n",
            )
            .with_file(&format!("{root}/commands/team/sync.md"), "Sync ${CLAUDE_PLUGIN_ROOT}\n"),
    );
    let kit = crate::plugins::Plugin {
        context: Arc::new(PluginContext {
            id: "kit@inline".to_owned(),
            name: "kit".to_owned(),
            root: root.to_owned(),
            data: Some("/Users/dev/.zcode/cli/plugins/data/kit@inline".to_owned()),
            options: std::collections::BTreeMap::from([
                (
                    "color".to_owned(),
                    PluginOption::Plain("!`rm -rf`".to_owned()),
                ),
                (
                    "token".to_owned(),
                    PluginOption::Secret(RedactedString::new("s3cret")),
                ),
            ]),
            declares_options: true,
        }),
        skill_dirs: vec![format!("{root}/skills")],
        command_roots: vec![crate::plugins::PluginCommandRoot::Dir(format!(
            "{root}/commands"
        ))],
        ..crate::test_support::plugin("kit", root)
    };
    let catalog = Arc::new(
        SkillCatalog::new(env.clone(), env.clone())
            .with_plugins(crate::test_support::plugin_set(vec![kit])),
    );
    let skills = catalog.skills(&crate::test_support::workspace(), &[]).await;
    let names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(&names[..2], ["kit:team:sync", "kit:notes"]);
    assert_eq!(skills[1].allowed_tools, [format!("Bash({root}/bin/x:*)")]);
    // 没写 description 的插件命令不进清单（照样能调用）。
    let listing = catalog
        .listing((&crate::test_support::workspace(), &[]), &[], "m")
        .await
        .entries;
    let listed: Vec<&str> = listing.iter().map(|entry| entry.name.as_str()).collect();
    assert!(listed.contains(&"kit:notes"), "{listed:?}");
    assert!(!listed.contains(&"kit:team:sync"), "{listed:?}");
    let ids: Arc<dyn Ids> = Arc::new(SequentialIds::default());
    let tool = SkillTool::new(catalog.clone(), ids, None);
    let context = tool_context(env, PermissionMode::DontAsk);
    let (progress, _ignored) = mpsc::channel(4);
    let call = tool
        .call_with_messages(json!({"skill": "kit:notes"}), context, progress)
        .await
        .unwrap();
    assert_eq!(
        body(&call),
        format!(
            "Base directory for this skill: {root}/skills/notes\n\nRoot {root}, data /Users/dev/.zcode/cli/plugins/data/kit@inline, color \\! `rm -rf`, key [sensitive option 'token' not available in skill content].\n"
        )
    );
}
