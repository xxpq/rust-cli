//! 设置页随界面语言（docs/specs/rust-cli/tui-kit.md §4.5「语言」，用户决定 2026-11-12）：中文界面里 `/settings` 的段名、行名
//! 与状态栏的键是中文；进一段、按 ↓ 选中一行时，行下面的说明与生效的时刻也是中文。
use crossterm::event::KeyCode;

use super::support::{GatedModel, Setup, with_ui};

#[tokio::test(flavor = "multi_thread")]
async fn the_settings_page_follows_the_chinese_screen() {
    let setup = Setup {
        width: 110,
        height: 36,
        lang: "zh_CN.UTF-8",
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("? 快捷键").await;
            ui.submit("/settings appearance");
            let screen = ui.wait_for("明暗").await;
            assert!(screen.contains("外观"), "{screen}");
            assert!(screen.contains("屏幕的颜色。"), "{screen}");
            assert!(screen.contains("esc 关闭"), "{screen}");
            assert!(!screen.contains("Light or dark"), "{screen}");
            // ↓ 选中「明暗」：它下面的说明换成它的。
            ui.key(KeyCode::Down);
            ui.wait_for("主题显示哪一面。").await;
        },
    )
    .await;
}
