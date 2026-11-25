//! `/stats` 打开的设置的 Stats 段（docs/specs/rust-cli/tui-panels.md §4.6、usage.md §5）：内核的本机活动统计（热力图、两列的数）
//! 与会话的 API 时长，⇥ 换范围，esc 关上设置；深浅主题。以前是 `/usage all` 的「This computer」页。
use crossterm::event::KeyCode;
use rust_cli_base::models::{GLM_4_6, GLM_5_3};
use rust_cli_kernel_api::metering::{ActivityDay, ActivityRange, ActivityStats, ModelTokens};
use rust_cli_tui::{ColorLevel, Colors};

use super::support::{GatedModel, Setup, with_ui};

/// 测试的时钟是 0：今天是 1970-01-01（周四）。
fn all_time() -> ActivityStats {
    ActivityStats {
        range: Some(ActivityRange::All),
        today: "1970-01-01".into(),
        days: vec![ActivityDay {
            date: "1970-01-01".into(),
            messages: 5,
        }],
        models: vec![
            ModelTokens {
                model: GLM_5_3.into(),
                input: 900,
                output: 100,
                cache_read: 0,
                cache_write: 0,
            },
            ModelTokens {
                model: GLM_4_6.into(),
                input: 40,
                output: 10,
                cache_read: 0,
                cache_write: 0,
            },
        ],
        sessions: 3,
        longest_session_ms: Some(65_000),
        first_session_ms: Some(0),
        last_session_ms: Some(0),
    }
}

/// 报告了前景（反色）的终端：rich 的主题（tui-kit.md §3.4）。
fn setup(lang: &'static str, background: (u8, u8, u8)) -> Setup {
    let foreground = (255 - background.0, 255 - background.1, 255 - background.2);
    let setup = Setup {
        height: 40,
        lang,
        colors: Colors::new(ColorLevel::TrueColor, Some(background))
            .with_foreground(Some(foreground)),
        ..Setup::default()
    };
    setup.store.set_activity(ActivityRange::All, all_time());
    setup.store.set_activity(
        ActivityRange::Week,
        ActivityStats {
            range: Some(ActivityRange::Week),
            sessions: 7,
            ..all_time()
        },
    );
    setup
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_shows_the_heat_map_the_figures_and_cycles_ranges() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model.clone(),
        setup("en_US.UTF-8", (0, 0, 0)),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/stats");
            let page = ui
                .wait("the figures", |s| {
                    s.contains("Settings  ›  Stats") && s.contains("Favorite model")
                })
                .await;
            // Puffin 的热力图（kit `heat_rows`）：段标题、半块的格子与图例。
            assert!(page.contains("Messages, last 52 weeks"), "{page}");
            assert!(page.contains("5 total"), "{page}");
            assert!(page.contains("\u{2580}"), "{page}");
            assert!(page.contains("Less ■ ■ ■ ■ More"), "{page}");
            assert!(
                page.contains("All time  ·  Last 7 days  ·  Last 30 days"),
                "{page}"
            );
            assert!(
                page.contains(&format!("Favorite model   {GLM_5_3}")),
                "{page}"
            );
            assert!(page.contains("Sessions         3"), "{page}");
            assert!(page.contains("This session"), "{page}");
            assert!(page.contains("API time"), "{page}");
            // 这一页的键在状态栏右边（设置页）。
            assert!(page.contains("⇥ range"), "{page}");
            // ⇥：Last 7 days 由内核查（MemoryStore 的预设）。
            ui.key(KeyCode::Tab);
            ui.wait_for("Sessions         7").await;
            // ⇥：Last 30 days 没有预设，内核给出空的统计。
            ui.key(KeyCode::Tab);
            ui.wait("the empty range", |s| {
                s.contains("No activity yet.") && !s.contains("Sessions         7")
            })
            .await;
            ui.key(KeyCode::Tab);
            ui.wait_for("Sessions         3").await;
            // esc 关上设置。
            ui.key(KeyCode::Esc);
            ui.wait("the settings to close", |s| !s.contains("Settings  ›"))
                .await;
        },
    )
    .await;
    assert!(
        model.requests().is_empty(),
        "/stats never reaches the model"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn light_backgrounds_choose_the_range_in_the_light_brand() {
    let model = GatedModel::scripted(Vec::new());
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = buffer.clone();
    with_ui(
        model,
        setup("en_US.UTF-8", (255, 255, 255)),
        |mut ui| async move {
            ui.submit("/stats");
            ui.wait_for("Last 30 days").await;
            *seen.lock().unwrap() = ui.shot.lock().unwrap().clone();
        },
    )
    .await;
    // 选中的范围是浅色主题的强调色（`Ink::accent`，`brand`），与深色主题的不同。
    let ink = |bg: (u8, u8, u8)| {
        rust_cli_tui::Ink::new(
            ColorLevel::TrueColor,
            Some((255 - bg.0, 255 - bg.1, 255 - bg.2)),
            Some(bg),
        )
    };
    let (light, dark) = (ink((255, 255, 255)).accent(), ink((0, 0, 0)).accent());
    assert_ne!(light, dark);
    let buffer = buffer.lock().unwrap().clone().unwrap();
    let area = buffer.area;
    let row = (area.top()..area.bottom())
        .find(|&y| {
            let text: String = (area.left()..area.right())
                .map(|x| buffer[(x, y)].symbol().to_owned())
                .collect();
            text.contains("Last 30 days")
        })
        .expect("the ranges");
    let text: Vec<String> = (area.left()..area.right())
        .map(|x| buffer[(x, row)].symbol().to_owned())
        .collect();
    let at = text
        .windows(3)
        .position(|w| w.concat() == "All")
        .expect("All time");
    assert_eq!(buffer[(area.left() + at as u16, row)].fg, light);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_library_says_there_is_no_activity_yet() {
    let model = GatedModel::scripted(Vec::new());
    let setup = Setup {
        height: 40,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("/stats");
        ui.wait("both empty sections", |s| {
            s.contains("Settings  ›  Stats") && s.matches("No activity yet.").count() == 2
        })
        .await;
    })
    .await;
}
