//! `/goal` 的卡（docs/specs/rust-cli/tui-kit.md §4.2）：目标在跑时每帧重画（Puffin `goal_moving`）。以前这里还有 `/stats` 数字卡的
//! 速度一行与结束了的子代理；2026-11-12 起 `/stats` 是设置的 Stats 段（tui-panels.md §4.6），卡删掉。
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::app::widgets::goal::{GoalState, SessionGoal};

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

/// 目标的卡开着、目标正在跑的这一段有开始时刻时每帧重画（Puffin `goal_moving`）；没有时不重画。
#[test]
fn the_goal_card_moves_only_while_a_stretch_is_running() {
    let mut app = app();
    let goal = |running_since: u64| SessionGoal {
        target_id: String::new(),
        objective: "tests pass".into(),
        title: String::new(),
        status: "active".into(),
        state: GoalState::Working,
        round: 1,
        time_used_seconds: 5,
        running_since,
        rounds: Vec::new(),
        reason: String::new(),
    };
    app.show_goal_card(Some(goal(0)), true);
    assert!(matches!(app.overlay, Some(Overlay::Card(Card::Goal(_)))));
    assert!(!app.cards_fading(std::time::Instant::now()));
    app.show_goal_card(Some(goal(1_000)), false);
    assert!(app.cards_fading(std::time::Instant::now()));
}
