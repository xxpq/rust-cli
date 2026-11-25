//! 嵌套目录的 skill（ext.md §2.6.1）：发现的目录、改名与描述、排在内置命令之前、宣布的内容。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, SystemMessage,
    ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::testing::MemoryEnvironment;
use serde_json::json;

use super::*;
use crate::test_support::{CWD, facts, workspace};

/// 一次 Read（`failed` 时结果是错误）。
fn read(index: usize, path: &str, failed: bool) -> Vec<Message> {
    let assistant = MessageId::new(format!("a{index}"));
    let id = ToolUseId::new(format!("t{index}"));
    vec![
        Message::Assistant(AssistantMessage {
            id: assistant.clone(),
            content: vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: "Read".to_owned(),
                input: json!({"file_path": path}),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new(format!("u{index}")),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id,
                content: vec![ToolResultContent::Text {
                    text: "x".to_owned(),
                }],
                is_error: failed,
            }],
            origin: UserOrigin::ToolResult { assistant },
        }),
    ]
}

fn skill(dir: &str, name: &str, description: &str) -> (String, String) {
    (
        format!("{CWD}/{dir}/{name}/SKILL.md"),
        format!("---\ndescription: {description}\n---\nBody of {name}.\n"),
    )
}

/// 夹具：cwd 有 `top`，`pkg/.zcode/skills` 有 `nested` 与同名的 `top`。
fn nested_fixture() -> SkillCatalog {
    let mut env = MemoryEnvironment::new(facts());
    for (path, text) in [
        skill(".zcode/skills", "top", "Top skill"),
        skill("pkg/.zcode/skills", "nested", "Nested skill"),
        skill("pkg/.zcode/skills", "top", "Package top"),
    ] {
        env = env.with_file(&path, &text);
    }
    let env = Arc::new(env.with_file(&format!("{CWD}/pkg/src/a.txt"), "hello\n"));
    SkillCatalog::new(env.clone(), env)
}

fn lines(listing: &SkillListing) -> Vec<&str> {
    listing
        .entries
        .iter()
        .map(|entry| entry.line.as_str())
        .collect()
}

#[tokio::test]
async fn a_read_under_cwd_adds_the_nested_skills() {
    let catalog = nested_fixture();
    let history = read(1, &format!("{CWD}/pkg/src/a.txt"), false);
    let listing = catalog.listing((&workspace(), &[]), &history, "").await;
    // 动态的一块按原名排（nested、top），排在内置的 `loop` 之后、`init` 之前（插在内置命令之前）。
    assert_eq!(
        lines(&listing),
        [
            "- top: Top skill",
            "- loop: Run a prompt or slash command on a recurring interval (e.g. /loop 5m /foo). Omit the interval to let the model self-pace. - When the user wants to set up a recurring task, poll for status, or run something repeatedly on an interval (e.g. \"check the deploy every 5 minutes\", \"keep running /babysit-prs\"). Do NOT invoke for one-off tasks.",
            "- nested: Nested skill (from pkg/.zcode/skills — applies when working on files under pkg/)",
            "- pkg:top: Package top (scoped to pkg/ — use this instead of the unscoped \"top\" skill when the files being changed are under pkg/)",
            "- init: Initialize a new AGENTS.md file with codebase documentation",
            "- security-review: Complete a security review of the pending changes on the current branch",
        ]
    );
    // 宣布：一个目录一条，名字是列目录的顺序（内存环境按名字），用合并之后的名字。
    assert_eq!(listing.dynamic.len(), 1);
    let dir = &listing.dynamic[0];
    assert_eq!(dir.dir, format!("{CWD}/pkg/.zcode/skills"));
    assert_eq!(dir.display, "pkg/.zcode/skills");
    assert_eq!(dir.names, ["nested", "pkg:top"]);
    // Skill 工具按改过的名字找到；正文与基础目录不变。
    let found = catalog
        .find((&workspace(), &[]), &history, "pkg:top")
        .await
        .unwrap();
    assert_eq!(
        found.dir.as_deref(),
        Some(format!("{CWD}/pkg/.zcode/skills/top").as_str())
    );
    assert_eq!(found.body, "Body of top.\n");
    // 不带前缀的 `top` 仍是 cwd 的那个。
    let top = catalog
        .find((&workspace(), &[]), &history, "top")
        .await
        .unwrap();
    assert_eq!(top.description, "Top skill");
}

#[tokio::test]
async fn nothing_is_added_without_a_successful_read_under_cwd() {
    let catalog = nested_fixture();
    let none = |history: Vec<Message>| {
        let catalog = &catalog;
        async move {
            let listing = catalog.listing((&workspace(), &[]), &history, "").await;
            (
                listing.dynamic.len(),
                lines(&listing).iter().any(|line| line.contains("nested")),
            )
        }
    };
    assert_eq!(none(Vec::new()).await, (0, false));
    // 失败的 Read 不算。
    assert_eq!(
        none(read(1, &format!("{CWD}/pkg/src/a.txt"), true)).await,
        (0, false)
    );
    // cwd 本身不算：读 cwd 下的文件只看到基础清单。
    assert_eq!(
        none(read(1, &format!("{CWD}/README.md"), false)).await,
        (0, false)
    );
    // cwd 外面的文件不算。
    assert_eq!(
        none(read(1, "/Users/dev/other/pkg/a.txt", false)).await,
        (0, false)
    );
    // 压缩边界之前的触及不算（同条件 skill，D6）。
    let mut history = read(1, &format!("{CWD}/pkg/src/a.txt"), false);
    history.push(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new("b1"),
            trigger: CompactTrigger::Auto,
            pre_tokens: 1,
            summary: MessageId::new("s1"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
    )));
    assert_eq!(none(history).await, (0, false));
}

#[tokio::test]
async fn same_named_nested_skills_are_both_scoped_deepest_dir_first() {
    let mut env = MemoryEnvironment::new(facts());
    for (path, text) in [
        skill("a/.agents/skills", "x", "Outer x"),
        skill("a/b/.zcode/skills", "x", "Inner x"),
        skill("a/b/.zcode/skills", "only", "Only"),
    ] {
        env = env.with_file(&path, &text);
    }
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    let history = read(1, &format!("{CWD}/a/b/c/file.rs"), false);
    let listing = catalog.listing((&workspace(), &[]), &history, "").await;
    let dynamic: Vec<&str> = lines(&listing)
        .into_iter()
        .filter(|line| line.contains("(from"))
        .collect();
    // 只与别的动态 skill 同名：两个都加前缀，描述用 “from” 那句；按原名、再按目录排。
    assert_eq!(
        dynamic,
        [
            "- only: Only (from a/b/.zcode/skills — applies when working on files under a/b/)",
            "- a:x: Outer x (from a/.agents/skills — applies when working on files under a/)",
            "- a/b:x: Inner x (from a/b/.zcode/skills — applies when working on files under a/b/)",
        ]
    );
    // 深的目录先宣布。
    let dirs: Vec<(&str, Vec<&str>)> = listing
        .dynamic
        .iter()
        .map(|dir| {
            (
                dir.display.as_str(),
                dir.names.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        dirs,
        [
            ("a/b/.zcode/skills", vec!["only", "a/b:x"]),
            ("a/.agents/skills", vec!["a:x"]),
        ]
    );
}

#[tokio::test]
async fn a_conditional_skill_in_a_nested_dir_is_listed_at_once() {
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{CWD}/pkg/.zcode/skills/ts/SKILL.md"),
        "---\ndescription: TS\npaths: '**/*.ts'\n---\n",
    );
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    let history = read(1, &format!("{CWD}/pkg/readme.md"), false);
    let listing = catalog.listing((&workspace(), &[]), &history, "").await;
    assert!(
        lines(&listing)
            .iter()
            .any(|line| line.starts_with("- ts: TS (from pkg/")),
        "{:?}",
        lines(&listing)
    );
}
