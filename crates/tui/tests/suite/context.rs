//! 设置的 Context 与 Status 段（docs/specs/rust-cli/tui-panels.md §4；设计稿 D）：`/context` 打开方格图与图例、一句话（计数来自
//! 内核），⏎ 进 Details 的大纲（逐行打开），esc 回到 Context；`/context all` 直接进 Details；`/status` 是自己的段。三个命令不是
//! 别名（2026-11-12）。各部分按本地估算（工作线 F93 起不发 count_tokens）。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, environments, text_response};

use super::support::{GatedModel, Setup, with_ui};

/// 有两个 skill 的工作区（明细里有 Skills 一段）。
fn with_skills() -> Setup {
    let skill = |what: &str| format!("---\ndescription: {what}\n---\nDo {what}.\n");
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    })
    .with_file(
        "/Users/dev/project/.zcode/skills/review/SKILL.md",
        &skill("review"),
    )
    .with_file(
        "/Users/dev/project/.zcode/skills/deploy/SKILL.md",
        &skill("deploy"),
    );
    Setup {
        width: 100,
        height: 50,
        environments: environments(Arc::new(env)),
        ..Setup::default()
    }
}

/// `/context` 打开设置的 Context 段：方格图、图例与一句话，状态栏是 `⏎ details`（没有以前的 `⇥ session`）；⏎ 进 Details：大纲
/// 逐行打开，esc 回到 Context，再 esc 关上设置；`/context all` 直接进 Details。
#[tokio::test(flavor = "multi_thread")]
async fn context_opens_its_section_and_enter_the_details() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    with_ui(model, with_skills(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/context");
        let screen = ui
            .wait("the grid's legend", |s| {
                s.contains("Settings  ›  Context") && s.contains("■ Skills")
            })
            .await;
        // 设置页代替正文，输入框（搜索）与状态栏留着。
        assert!(screen.contains("Ask before changes"), "{screen}");
        assert!(screen.contains(" of 200k   "), "{screen}");
        assert!(screen.contains("comes with every request"), "{screen}");
        assert!(screen.contains("⏎ details"), "{screen}");
        assert!(!screen.contains("session"), "{screen}");
        assert!(
            !screen.contains("▸"),
            "the outline is in the details: {screen}"
        );
        ui.key(KeyCode::Enter);
        let details = ui.wait_for("Settings  ›  Context  ›  Details").await;
        assert!(details.contains("⏎ expand"), "{details}");
        // 打开 Skills：工作区的两个 skill 与内置的三个，note 是来源的小写。
        ui.key(KeyCode::End);
        let open = ui.wait("Skills chosen", |s| s.contains("▸ ■ Skills")).await;
        assert!(open.contains("5 skills"), "{open}");
        ui.key(KeyCode::Right);
        let open = ui.wait("the skills", |s| s.contains("▾ ■ Skills")).await;
        let review = open
            .lines()
            .find(|l| l.contains(" review "))
            .unwrap_or_else(|| panic!("{open}"));
        assert!(review.contains("project"), "{review}");
        assert!(open.contains("⏎ collapse"), "{open}");
        // esc 回到 Context，再 esc 关上设置。
        ui.key(KeyCode::Esc);
        ui.wait("back to the grid", |s| {
            s.contains("Settings  ›  Context") && !s.contains("Details")
        })
        .await;
        ui.key(KeyCode::Esc);
        ui.wait("the settings to close", |s| !s.contains("Settings  ›"))
            .await;
        // `/context all` 直接进 Details；esc 回到 Context。
        ui.submit("/context all");
        ui.wait_for("Settings  ›  Context  ›  Details").await;
        ui.key(KeyCode::Esc);
        ui.wait("back to the grid", |s| {
            s.contains("Settings  ›  Context") && !s.contains("Details")
        })
        .await;
    })
    .await;
}

/// `/status` 打开设置的 Status 段（tui-panels.md §4.5）：行来自真实的内核与界面（会话 ID、回合、目录、版本、MCP 的事实）；
/// 状态栏是 `⏎ copy id`，没有以前的 `⇥ context`；esc 关上设置。
#[tokio::test(flavor = "multi_thread")]
async fn status_opens_its_section() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    with_ui(model, with_skills(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 还没有会话：Session 说还没开始，状态栏没有 ⏎。
        ui.submit("/status");
        let fresh = ui.wait_for("Settings  ›  Status").await;
        assert!(fresh.contains("not started"), "{fresh}");
        assert!(!fresh.contains("copy id"), "{fresh}");
        assert!(!fresh.contains("context"), "{fresh}");
        ui.key(KeyCode::Esc);
        ui.wait("the settings to close", |s| !s.contains("Settings  ›"))
            .await;
        ui.submit("hello");
        ui.wait_for("ok").await;
        ui.submit("/status");
        let screen = ui
            .wait("the session's facts", |s| {
                s.contains("Settings  ›  Status") && s.contains("No MCP servers")
            })
            .await;
        assert!(screen.contains("1 turn"), "{screen}");
        assert!(screen.contains("Directory  ~/project"), "{screen}");
        assert!(screen.contains("Version    zcode "), "{screen}");
        assert!(screen.contains("Mode       Ask before changes"), "{screen}");
        assert!(screen.contains("⏎ copy id"), "{screen}");
        ui.key(KeyCode::Esc);
        ui.wait("the settings to close", |s| !s.contains("Settings  ›"))
            .await;
    })
    .await;
}
