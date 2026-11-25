//! ← 打开任务列表的手势（tui-cockpit.md §3.5）：经 `App::dispatch`，时刻都是给定的按键时刻。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::app::App;
use crate::ui::composer::InputMode;
use crate::ui::test_support::app;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn press(app: &mut App, code: KeyCode, at: Instant) {
    let _ = app.dispatch(KeyEvent::new(code, KeyModifiers::NONE), at);
}

/// 按住 ← 的连发：第一下打开，后面相隔 30 ms 的几下不算，任务列表留着。修复前第二下就“回去”了（开了又关）。
#[test]
fn holding_left_opens_the_task_list_once() {
    let mut app = app();
    let t0 = Instant::now();
    press(&mut app, KeyCode::Left, t0);
    assert!(app.cockpit_open());
    for i in 1..=5 {
        press(&mut app, KeyCode::Left, t0 + ms(30 * i));
        assert!(app.cockpit_open(), "repeat {i} must not close it");
    }
    // 松开之后、打开 1 s 之后的一下才是回去。
    press(&mut app, KeyCode::Left, t0 + ms(1_500));
    assert!(!app.cockpit_open());
}

/// 刚删空草稿（2 s 内）：第一下只提示“再按一次”，3 s 内再按一下才打开；过了 2 s 的一下直接打开。
#[test]
fn left_right_after_emptying_the_draft_asks_for_a_second_press() {
    let mut app = app();
    let t0 = Instant::now();
    press(&mut app, KeyCode::Char('a'), t0);
    press(&mut app, KeyCode::Backspace, t0 + ms(100));
    press(&mut app, KeyCode::Left, t0 + ms(400));
    assert!(!app.cockpit_open());
    let hint = app.notice_text(t0 + ms(400)).unwrap_or_default();
    assert!(hint.contains("again"), "{hint:?}");
    press(&mut app, KeyCode::Left, t0 + ms(900));
    assert!(app.cockpit_open());

    let mut later = crate::ui::test_support::app();
    press(&mut later, KeyCode::Char('a'), t0);
    press(&mut later, KeyCode::Backspace, t0 + ms(100));
    press(&mut later, KeyCode::Left, t0 + ms(2_500));
    assert!(later.cockpit_open());
}

/// 提示之后按了别的键：提示作废，下一下 ← 重新判断（仍在删空后的 2 s 里就再提示一次）。
#[test]
fn another_key_cancels_the_armed_left() {
    let mut app = app();
    let t0 = Instant::now();
    press(&mut app, KeyCode::Char('a'), t0);
    press(&mut app, KeyCode::Backspace, t0 + ms(100));
    press(&mut app, KeyCode::Left, t0 + ms(300));
    press(&mut app, KeyCode::Down, t0 + ms(500));
    press(&mut app, KeyCode::Left, t0 + ms(800));
    assert!(!app.cockpit_open());
}

/// 打开之后按了别的键（⏎、↓），马上再按 ← 是新的一下：不当作按住不放（修复前的套件 projects、resume_page）。
#[test]
fn another_key_between_makes_the_next_left_count() {
    let mut app = app();
    let t0 = Instant::now();
    press(&mut app, KeyCode::Left, t0);
    assert!(app.cockpit_open());
    press(&mut app, KeyCode::Down, t0 + ms(200));
    press(&mut app, KeyCode::Left, t0 + ms(400));
    assert!(!app.cockpit_open());
}

/// 发送清空了草稿：马上按 ← 直接打开（草稿里本来就没有要移的字；修复前的套件 resume_page 是这样用的）。
#[test]
fn left_right_after_sending_opens_at_once() {
    let mut app = app();
    let t0 = Instant::now();
    press(&mut app, KeyCode::Char('a'), t0);
    press(&mut app, KeyCode::Enter, t0 + ms(100));
    press(&mut app, KeyCode::Left, t0 + ms(300));
    assert!(app.cockpit_open());
}

/// `!` 模式里草稿空着时 ← 不打开（§3.4 的条件；修复前会打开）。
#[test]
fn left_in_shell_mode_does_not_open_the_task_list() {
    let mut app = app();
    app.composer.mode = InputMode::Bash;
    press(&mut app, KeyCode::Left, Instant::now());
    assert!(!app.cockpit_open());
}
