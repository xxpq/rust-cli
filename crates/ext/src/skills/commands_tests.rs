//! 旧的 `commands/` 目录（ext.md §2.10）：发现、名字、顺序、frontmatter 与展开。只用目录与展开的公开入口，旧代码上照样编得过、断言失败。

use std::sync::Arc;

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};
use rust_cli_kernel_api::user_command::{
    ExpandedKind, ExpandedMessage, Expansion, UserCommandKind, UserCommands,
};

use super::*;
use crate::skills::runner;
use crate::test_support::{AllowedGate, CWD, EchoBash, HOME, facts, tool_context, workspace};

const MANAGED: &str = "/Library/Application Support/ZCode";

fn catalog(env: MemoryEnvironment) -> SkillCatalog {
    let env = Arc::new(env);
    SkillCatalog::new(env.clone(), env)
}

/// 发现的条目（不含内置）：`名字=描述`。
fn names(skills: &[Arc<Skill>]) -> Vec<String> {
    skills
        .iter()
        .filter(|skill| skill.source != SkillSource::Builtin)
        .map(|skill| format!("{}={}", skill.name, skill.description))
        .collect()
}

fn command(description: &str) -> String {
    format!("---\ndescription: {description}\n---\nBody.\n")
}

#[tokio::test]
async fn commands_come_from_every_level_after_the_skills_and_are_sorted_by_name() {
    let env = MemoryEnvironment::new(facts())
        .with_file(
            &format!("{MANAGED}/.zcode/commands/zeta.md"),
            &command("managed"),
        )
        .with_file(
            &format!("{HOME}/.zcode/commands/beta.md"),
            &command("user zcode"),
        )
        .with_file(
            &format!("{HOME}/.agents/commands/alpha.md"),
            &command("user agents"),
        )
        .with_file(
            &format!("{CWD}/.zcode/commands/beta.md"),
            &command("project"),
        )
        .with_file(
            &format!("{CWD}/.agents/commands/gamma.md"),
            &command("project agents"),
        )
        .with_file(
            &format!("{CWD}/.zcode/skills/delta/SKILL.md"),
            &command("project skill"),
        );
    let skills = catalog(env).skills(&workspace(), &[]).await;
    // skill 在前；命令整体按名字排，同名时先收的（用户级）留下
    assert_eq!(
        names(&skills),
        [
            "delta=project skill",
            "alpha=user agents",
            "beta=user zcode",
            "gamma=project agents",
            "zeta=managed",
        ]
    );
    // 来源（ext.md §2.7）：`skills/` 目录的是 skill，`commands/` 目录的是命令
    let kinds: Vec<(&str, UserCommandKind)> = skills
        .iter()
        .filter(|skill| skill.source != SkillSource::Builtin)
        .map(|skill| (skill.name.as_str(), skill.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("delta", UserCommandKind::Skill),
            ("alpha", UserCommandKind::Command),
            ("beta", UserCommandKind::Command),
            ("gamma", UserCommandKind::Command),
            ("zeta", UserCommandKind::Command),
        ]
    );
}

#[tokio::test]
async fn a_skill_wins_over_a_command_with_the_same_name() {
    let env = MemoryEnvironment::new(facts())
        .with_file(
            &format!("{HOME}/.zcode/commands/deploy.md"),
            &command("command"),
        )
        .with_file(
            &format!("{CWD}/.zcode/skills/deploy/SKILL.md"),
            &command("skill"),
        );
    let skills = catalog(env).skills(&workspace(), &[]).await;
    assert_eq!(names(&skills), ["deploy=skill"]);
}

#[tokio::test]
async fn names_join_the_subdirectories_with_colons() {
    let root = format!("{CWD}/.zcode/commands");
    let env = MemoryEnvironment::new(facts())
        .with_file(
            &format!("{root}/frontend/component.md"),
            &command("component"),
        )
        .with_file(&format!("{root}/frontend/ui/Button.md"), &command("button"))
        .with_file(&format!("{root}/.hidden/secret.md"), &command("hidden"))
        .with_file(&format!("{root}/notes.txt"), "not markdown")
        .with_file(&format!("{root}/upper.MD"), &command("not matched"))
        // 有 SKILL.md 的目录只取它；名字是这个目录（连同上层的命名空间）
        .with_file(
            &format!("{root}/tools/deploy/SKILL.md"),
            &command("skill format"),
        )
        .with_file(
            &format!("{root}/tools/deploy/extra.md"),
            &command("dropped"),
        )
        .with_file(
            &format!("{root}/tools/deploy/nested/inner.md"),
            &command("nested"),
        );
    let skills = catalog(env).skills(&workspace(), &[]).await;
    assert_eq!(
        names(&skills),
        [
            ".hidden:secret=hidden",
            "frontend:component=component",
            "frontend:ui:Button=button",
            "tools:deploy=skill format",
            "tools:deploy:nested:inner=nested",
        ]
    );
}

#[tokio::test]
async fn a_skill_file_at_the_root_is_named_after_the_root() {
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{HOME}/.zcode/commands/SKILL.md"),
        &command("root skill"),
    );
    let skills = catalog(env).skills(&workspace(), &[]).await;
    assert_eq!(names(&skills), ["commands=root skill"]);
}

#[tokio::test]
async fn files_over_the_limit_and_deep_trees_are_skipped() {
    let root = format!("{HOME}/.zcode/commands");
    let big = format!("---\ndescription: big\n---\n{}", "x".repeat(1_048_577));
    let deep = (0..12)
        .map(|level| format!("d{level}"))
        .collect::<Vec<_>>()
        .join("/");
    let deeper = format!("{deep}/d12");
    let env = MemoryEnvironment::new(facts())
        .with_file(&format!("{root}/big.md"), &big)
        .with_file(&format!("{root}/{deep}/kept.md"), &command("kept"))
        .with_file(&format!("{root}/{deeper}/lost.md"), &command("lost"));
    let skills = catalog(env).skills(&workspace(), &[]).await;
    assert_eq!(
        names(&skills),
        [format!("{}:kept=kept", deep.replace('/', ":"))]
    );
}

#[tokio::test]
async fn frontmatter_name_and_paths_are_not_read_and_the_fallback_is_custom_command() {
    let root = format!("{HOME}/.zcode/commands");
    let env = MemoryEnvironment::new(facts())
        .with_file(
            &format!("{root}/named.md"),
            "---\nname: Shown\npaths: src/**\nargument-hint: <scope>\n---\n# Heading line\n\nBody.\n",
        )
        .with_file(&format!("{root}/empty.md"), "---\nwhen_to_use: Never\n---\n");
    let skills = catalog(env).skills(&workspace(), &[]).await;
    let named = skills.iter().find(|skill| skill.name == "named").unwrap();
    assert_eq!(named.display, None);
    assert_eq!(named.paths, None);
    assert_eq!(named.description, "Heading line");
    assert_eq!(named.argument_hint.as_deref(), Some("<scope>"));
    let empty = skills.iter().find(|skill| skill.name == "empty").unwrap();
    assert_eq!(empty.description, "Custom command");
}

#[tokio::test]
async fn additional_directories_add_their_commands_after_the_workspace() {
    let extra = "/Users/dev/extra";
    let env = MemoryEnvironment::new(facts())
        .with_file(
            &format!("{extra}/.zcode/commands/beta.md"),
            &command("extra"),
        )
        .with_file(
            &format!("{CWD}/.zcode/commands/beta.md"),
            &command("project"),
        )
        .with_file(
            &format!("{extra}/.agents/commands/omega.md"),
            &command("extra agents"),
        );
    let skills = catalog(env)
        .skills(&workspace(), &[EnvPath::new(extra)])
        .await;
    assert_eq!(names(&skills), ["beta=project", "omega=extra agents"]);
}

// ---------- 展开（§2.4、§2.5、§2.7） ----------

fn input(text: &str) -> ExpandedMessage {
    ExpandedMessage {
        text: text.to_owned(),
        kind: ExpandedKind::Input,
    }
}

fn meta(text: &str) -> ExpandedMessage {
    ExpandedMessage {
        text: text.to_owned(),
        kind: ExpandedKind::Meta {
            source: SKILL_MESSAGE_SOURCE.to_owned(),
        },
    }
}

async fn expand(env: MemoryEnvironment, text: &str) -> Expansion {
    let env = Arc::new(env);
    let catalog = Arc::new(SkillCatalog::new(env.clone(), env.clone()));
    let ids: Arc<dyn Ids> = Arc::new(SequentialIds::default());
    let commands = SkillCommands::new(
        catalog,
        Some(runner(
            Arc::new(EchoBash::default()),
            Arc::new(AllowedGate::default()),
            ids,
        )),
    );
    commands
        .expand(text, tool_context(env, PermissionMode::DontAsk))
        .await
}

#[tokio::test]
async fn a_markdown_command_expands_without_a_base_directory() {
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{CWD}/.zcode/commands/frontend/component.md"),
        "Create a $0 component ($ARGUMENTS).\nDir: ${CLAUDE_SKILL_DIR}\nProject: ${ZCODE_PROJECT_DIR}\n",
    );
    assert_eq!(
        expand(env, "/frontend:component Button primary").await,
        Expansion::Messages {
            messages: vec![
                input(
                    "<command-message>frontend:component</command-message>\n<command-name>/frontend:component</command-name>\n<command-args>Button primary</command-args>"
                ),
                meta(&format!(
                    "Create a Button component (Button primary).\nDir: ${{CLAUDE_SKILL_DIR}}\nProject: {CWD}\n"
                )),
            ],
            query: true,
        }
    );
}

#[tokio::test]
async fn a_command_without_placeholders_gets_the_arguments_appended() {
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{HOME}/.zcode/commands/hello.md"),
        "---\ndescription: Say hello\n---\nSay hello.\n",
    );
    assert_eq!(
        expand(env, "/hello Ada").await,
        Expansion::Messages {
            messages: vec![
                input(
                    "<command-message>hello</command-message>\n<command-name>/hello</command-name>\n<command-args>Ada</command-args>"
                ),
                meta("Say hello.\n\n\nARGUMENTS: Ada"),
            ],
            query: true,
        }
    );
}

#[tokio::test]
async fn a_skill_format_command_keeps_its_base_directory() {
    let dir = format!("{CWD}/.zcode/commands/tools/deploy");
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{dir}/SKILL.md"),
        "Deploy from ${CLAUDE_SKILL_DIR}.\n",
    );
    assert_eq!(
        expand(env, "/tools:deploy").await,
        Expansion::Messages {
            messages: vec![
                input(
                    "<command-message>tools:deploy</command-message>\n<command-name>/tools:deploy</command-name>"
                ),
                meta(&format!(
                    "Base directory for this skill: {dir}\n\nDeploy from {dir}.\n"
                )),
            ],
            query: true,
        }
    );
}
