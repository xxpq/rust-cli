//! 上下文的计数与设置的 Context、Details（tui-panels.md §4.1–§4.3）：这几页显示时才问，答案只收现在看的会话的，数变了的
//! 淡入（第一次的答案不淡）；Context 上 ⏎ 进 Details、esc 回到 Context、再 esc 关上设置；`/context all` 直接进 Details。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::StyledLine;

use super::*;
use crate::ui::app::Action;
use crate::ui::context_tree::tests::sample;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::settings::page::Page;
use crate::view::Transcript;

fn app() -> App {
    let mut app = App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.session = Some("sess_main".into());
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// 设置的整屏（100 × 40），一行一个字符串。
fn screen(app: &mut App) -> Vec<String> {
    app.settings_frame(Instant::now(), 0, 100, 40)
        .expect("the settings")
        .iter()
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .collect()
}

fn counted(app: &mut App) {
    let stale = app.context_counted(
        Some(SessionId::new("sess_main")),
        Ok(sample()),
        Instant::now(),
        0,
    );
    assert!(!stale);
}

fn page(app: &App) -> Page {
    app.settings.as_ref().expect("the settings").page()
}

/// Context、Details 与 Status 显示时才问，问的是主会话；别的段、关上设置、查看子代理时不问。
#[test]
fn the_count_is_asked_while_context_or_status_is_shown() {
    let mut app = app();
    assert_eq!(app.context_wanted(), None);
    assert_eq!(app.slash("/context"), Some(Action::CountContext));
    assert_eq!(
        app.context_wanted(),
        Some(Some(SessionId::new("sess_main")))
    );
    let _ = app.slash("/status");
    assert!(app.context_wanted().is_some());
    let _ = app.slash("/stats");
    assert_eq!(app.context_wanted(), None);
    let _ = app.slash("/settings");
    assert_eq!(app.context_wanted(), None);
    app.settings_close();
    assert_eq!(app.context_wanted(), None);
}

/// 答案问的不是现在看的会话（切了会话）：丢掉，要 runner 再问。
#[test]
fn an_answer_for_another_session_is_dropped() {
    let mut app = app();
    let _ = app.slash("/context");
    let stale = app.context_counted(
        Some(SessionId::new("sess_old")),
        Ok(sample()),
        Instant::now(),
        0,
    );
    assert!(stale);
    assert!(app.context_now().is_none());
}

/// 第一次的答案不淡；之后变了的数（按路径，图例按组）淡入，没变的不淡；Context 显示时设置按动画节拍重画。
#[test]
fn changed_numbers_fade_but_the_first_answer_does_not() {
    let mut app = app();
    let _ = app.slash("/context");
    let session = Some(SessionId::new("sess_main"));
    app.context_counted(session.clone(), Ok(sample()), Instant::now(), 0);
    assert!(app.context_count.changed.is_empty());
    let mut next = sample();
    for c in &mut next.categories {
        if c.name == "Messages" {
            c.tokens += 1_000;
        }
    }
    next.messages[1].tokens += 1_000;
    app.context_counted(session, Ok(next), Instant::now(), 10);
    let keys: Vec<&str> = app
        .context_count
        .changed
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["legend/messages", "messages", "messages/Replies"]);
    assert!(app.settings_moving(Instant::now()));
}

/// 答案到之前写 `Counting…`，失败时写原因，到了是方格图与图例；面包屑是 `Settings › Context`，没有以前的 `Stats › Context`。
#[test]
fn context_counts_then_fails_or_shows_the_grid() {
    let mut app = app();
    let _ = app.slash("/context");
    let text = screen(&mut app);
    assert!(text[1].contains("Settings  ›  Context"), "{text:#?}");
    assert!(text.iter().any(|l| l.ends_with("Counting…")), "{text:#?}");
    app.context_counted(
        Some(SessionId::new("sess_main")),
        Err("the kernel is busy".into()),
        Instant::now(),
        0,
    );
    assert!(
        screen(&mut app)
            .iter()
            .any(|l| l.contains("✗ the kernel is busy"))
    );
    counted(&mut app);
    let text = screen(&mut app);
    assert!(
        text.iter().any(|l| l.contains("126k of 200k   62%")),
        "{text:#?}"
    );
    // 大纲不在 Context 上：它在 Details。
    assert!(!text.iter().any(|l| l.contains("▸")), "{text:#?}");
}

/// Context 的状态栏是 `⏎ details`，没有以前的 `⇥ session`；⏎ 进 Details（面包屑多一级、大纲、`⏎ expand`），→ 打开、
/// 状态栏换 `⏎ collapse`；esc 回到 Context，再 esc 关上设置；再进 Details 时开合还在（设置开着的期间）。
#[test]
fn enter_opens_the_details_and_esc_goes_back() {
    let mut app = app();
    let _ = app.slash("/context");
    counted(&mut app);
    // 状态栏第 1 行（这一页的键）是倒数第 2 行，最后一行是目录与标题（tui-display.md §2.6）。
    let last = |app: &mut App| screen(app).iter().rev().nth(1).cloned().unwrap_or_default();
    let keys = last(&mut app);
    assert!(keys.contains("⏎ details"), "{keys}");
    assert!(!keys.contains("session"), "{keys}");
    app.settings_key(key(KeyCode::Enter));
    assert_eq!(page(&app), Page::ContextDetails);
    let text = screen(&mut app);
    assert!(
        text[1].contains("Settings  ›  Context  ›  Details"),
        "{text:#?}"
    );
    assert!(text.iter().any(|l| l.contains("▸")), "{text:#?}");
    assert!(last(&mut app).contains("⏎ expand"));
    app.settings_key(key(KeyCode::Right));
    assert!(last(&mut app).contains("⏎ collapse"));
    // ← 在开着的行上合上它，不回段目录。
    app.settings_key(key(KeyCode::Left));
    assert!(last(&mut app).contains("⏎ expand"));
    assert!(!app.settings.as_ref().unwrap().in_list);
    app.settings_key(key(KeyCode::Right));
    app.settings_key(key(KeyCode::Esc));
    assert_eq!(page(&app), Page::Top("context"));
    app.settings_key(key(KeyCode::Enter));
    assert_eq!(app.settings.as_ref().unwrap().context.open.len(), 1);
    app.settings_key(key(KeyCode::Esc));
    app.settings_key(key(KeyCode::Esc));
    assert!(app.settings.is_none());
}

/// `/context all` 直接进 Details（展开明细）；esc 回到 Context。
#[test]
fn context_all_opens_the_details() {
    let mut app = app();
    assert_eq!(app.slash("/context all"), Some(Action::CountContext));
    assert_eq!(page(&app), Page::ContextDetails);
    app.settings_key(key(KeyCode::Esc));
    assert_eq!(page(&app), Page::Top("context"));
}

/// 查看子代理时 Context 只说一句，⏎ 不进 Details。
#[test]
fn context_while_viewing_a_subagent_is_one_sentence() {
    let mut app = app();
    app.viewing = Some(crate::ui::agents::Viewing {
        agent: rust_cli_kernel_api::ids::AgentId::new("a1"),
        kind: "explore".into(),
        description: "look around".into(),
        version: 0,
        status: rust_cli_kernel_api::panels::SubagentStatus::Running,
        view: Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        messages: Vec::new(),
        owner: None,
    });
    let _ = app.slash("/context");
    assert_eq!(app.context_wanted(), None);
    let text = screen(&mut app);
    assert!(
        text.iter()
            .any(|l| l.ends_with("Context is counted for the main session only.")),
        "{text:#?}"
    );
    app.settings_key(key(KeyCode::Enter));
    assert_eq!(page(&app), Page::Top("context"));
}

/// 要不要再画一帧按画出的那一帧的时刻判断（tui.md §4.1）：Context 上刚变的数在那一帧里还在淡入，墙上时钟已过去 1 秒。
#[test]
fn a_context_figure_still_fading_in_the_drawn_frame_is_drawn_again() {
    let mut app = app();
    let _ = app.slash("/context");
    let changed = Instant::now() - std::time::Duration::from_secs(1);
    app.context_count
        .changed
        .insert("messages".to_owned(), (0, changed));
    let tick = std::time::Duration::from_millis(TICK_MS);
    assert!(app.animating(changed + tick));
    assert!(!app.animating(changed + tick * fade::TICKS.unsigned_abs() as u32));
}
