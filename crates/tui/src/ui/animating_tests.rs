//! 要不要再画一帧按画出的那一帧的时刻判断（tui.md §4.1）：淡入或限时提示在那一帧里还没落定，就再画一帧，不看判断时的
//! 墙上时钟。下面的帧都画在过去：按墙上时钟早已落定，按帧的时刻还没有。
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::ui::clipboard::Notice;
use crate::ui::composer::InputMode;
use crate::ui::notice::DOUBLE;
use crate::ui::settings::Settings;
use crate::ui::test_support::app;

/// 淡入的时长（kit `fade::TICKS` 个 tick）。
const FADE: Duration = Duration::from_millis(
    rust_cli_tui_kit::app::widgets::playhead::TICK_MS
        * rust_cli_tui_kit::app::widgets::fade::TICKS.unsigned_abs(),
);
/// 帧画在淡入开始之后 1 个 tick。
const ONE_TICK: Duration = Duration::from_millis(rust_cli_tui_kit::app::widgets::playhead::TICK_MS);

/// 帧画在淡入开始之后 1 个 tick（4 个 tick 才落定）；墙上时钟已过去 1 秒。
#[test]
fn a_slot_still_fading_in_the_drawn_frame_is_drawn_again() {
    let mut app = app();
    let changed = Instant::now() - Duration::from_secs(1);
    app.slot_seen = Some(("? keys".to_owned(), changed));
    let painted = changed + Duration::from_millis(33);
    assert!(app.animating(painted));
    // 帧画在落定之后：不再重画。
    assert!(!app.animating(changed + Duration::from_millis(200)));
}

/// 连按 esc 的提示在画出的那一帧里还在（800 ms 内）；墙上时钟已过了它的时限。
#[test]
fn a_notice_still_shown_in_the_drawn_frame_is_drawn_again() {
    let mut app = app();
    let pressed = Instant::now() - Duration::from_secs(1);
    app.notify("esc rewind", DOUBLE);
    if let Some(notice) = app.notice.as_mut() {
        notice.until = pressed + DOUBLE;
    }
    assert!(app.animating(pressed + Duration::from_millis(700)));
    assert!(!app.animating(pressed + DOUBLE));
}

/// 复制的提示（盖在 gap 行右侧）在画出的那一帧里还在；墙上时钟已过了它的时长。
#[test]
fn a_copy_notice_still_shown_in_the_drawn_frame_is_drawn_again() {
    let mut app = app();
    let copied = Instant::now() - Duration::from_secs(10);
    let duration = Duration::from_secs(2);
    app.pointer.copied = Some((
        Notice {
            text: "Copied 5 characters".to_owned(),
            duration,
        },
        copied,
    ));
    assert!(app.animating(copied + Duration::from_millis(100)));
    assert!(!app.animating(copied + duration));
}

/// 设置页刚变的值在画出的那一帧里还在淡入；墙上时钟已过去 1 秒。
#[test]
fn a_settings_value_still_fading_in_the_drawn_frame_is_drawn_again() {
    let mut app = app();
    let changed = Instant::now() - Duration::from_secs(1);
    app.settings = Some(Settings {
        typed: BTreeMap::from([("general/theme".to_owned(), Some(changed))]),
        ..Settings::default()
    });
    assert!(app.animating(changed + ONE_TICK));
    assert!(!app.animating(changed + FADE));
}

/// 输入框的 `!`（bash 模式）在画出的那一帧里还在淡入；墙上时钟已过去 1 秒。
#[test]
fn an_input_fade_still_running_in_the_drawn_frame_is_drawn_again() {
    let mut app = app();
    let entered = Instant::now() - Duration::from_secs(1);
    app.composer.mode = InputMode::Bash;
    crate::ui::input_motion::note(&mut app, entered, 0);
    assert!(app.animating(entered + ONE_TICK));
    assert!(!app.animating(entered + FADE));
}

/// `/model` 的档位名在 ← → 之后从板上淡入（kit `effort_bars_said`）。缺陷：以前 `animating` 不看它，换档位那一帧画出的名字
/// 还是板的底色（看不见），之后没有重画，要等鼠标之类的事件才出现。修好后淡入完之前一直重画。
#[test]
fn the_effort_name_of_the_model_picker_fades_in_after_an_arrow() {
    use crossterm::event::KeyCode;
    use rust_cli_base::models::GLM_5_ACCOUNT;
    use rust_cli_kernel_api::model::ModelSelection;

    use crate::ui::app::Overlay;
    use crate::ui::list::{Choice, Item, ListView, Purpose};
    use crate::ui::test_support::key;
    use crate::ui::transcript::Cache;

    let mut app = app();
    let item = Item {
        label: GLM_5_ACCOUNT.into(),
        description: "Z.ai".into(),
        current: true,
        value: Choice::Model(ModelSelection::new("zai", GLM_5_ACCOUNT)),
        levels: vec!["low".into(), "medium".into(), "high".into()],
        level: 0,
        tag: String::new(),
    };
    app.overlay = Some(Overlay::List(ListView::new(
        Purpose::Model,
        "Select model".into(),
        vec![item],
        "none".into(),
    )));
    let mut cache = Cache::default();
    let opened = app.started + Duration::from_secs(1);
    let _ = crate::ui::draw::draw(&mut app, &mut cache, opened, 0, 100, 40);
    // 打开时档位名已经落定，什么都不动。
    assert!(!crate::ui::draw::animating(&app, &cache, opened));
    if let Some(Overlay::List(list)) = app.overlay.as_mut() {
        list.on_key(key(KeyCode::Right));
    }
    let pressed = opened + ONE_TICK;
    let _ = crate::ui::draw::draw(&mut app, &mut cache, pressed, 0, 100, 40);
    assert!(
        crate::ui::draw::animating(&app, &cache, pressed),
        "the effort name is still fading in, so another frame is needed"
    );
    let settled = pressed + FADE;
    let _ = crate::ui::draw::draw(&mut app, &mut cache, settled, 0, 100, 40);
    assert!(!crate::ui::draw::animating(&app, &cache, settled));
}
