//! 输入层的卡（docs/specs/rust-cli/tui-kit.md §4.2「目标（/goal）」「用量（/usage）」；Puffin `shell/layer_cards.rs`、
//! `goals.rs`）：`/goal` 与 `/usage` 打开的卡在输入框的板上，草稿空着时开着；打字关上它、字进草稿，esc 关上；目标的卡 ⏎ 暂停
//! 或继续。卡的行是 kit 的 `goal::card` 与 `usage_plans.rs`。
//!
//! 以前还有 `/stats`（别名 `/status`、`/context`）的数字卡与它打开的统计的页；2026-11-12 起三个命令是设置页的段
//! （docs/specs/rust-cli/tui-panels.md §4，`settings/live.rs`），卡删掉。
//!
//! 目标是 runner 从会话取的 `GoalView`，按 Puffin host 的 `goal_of` 换成 kit 的 [`SessionGoal`]。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::goal::GoalView;
use rust_cli_tui_kit::app::element::stack;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::goal::{
    self as kit_goal, GoalRound, GoalState, GoalVerdict, Parts,
};
use rust_cli_tui_kit::app::widgets::key_hints;
use rust_cli_tui_kit::render::{Span, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

pub use rust_cli_tui_kit::app::widgets::goal::SessionGoal;

use super::app::{Action, App, Overlay};

/// 开着的卡。
#[derive(Debug, Clone, PartialEq)]
pub enum Card {
    /// 目标；没有目标时为 `None`（卡说怎样设一个）。
    Goal(Option<SessionGoal>),
    /// `/usage` 的卡：第几个条目（usage_plans.rs）。
    Usage { entry: usize },
    /// `/usage` 的详情页（代替正文，`page_frame.rs`）：条目、范围、顶行。
    UsagePage {
        entry: usize,
        range: usize,
        top: usize,
    },
}

/// `GoalView` → kit 的目标（Puffin host `goal_of`）：在跑的目标是第几轮，结束了的是它的结论。
pub fn goal_of(view: &GoalView, now_ms: u64) -> Option<SessionGoal> {
    let rounds = |round: u32, step: &str, last: GoalVerdict| {
        (1..=round)
            .map(|r| GoalRound {
                round: r,
                step: if r == 1 {
                    step.to_owned()
                } else {
                    String::new()
                },
                verdict: if r < round { GoalVerdict::NotYet } else { last },
                done: 0,
                total: 0,
            })
            .collect()
    };
    if let Some(active) = &view.active {
        let round = active.iterations + 1;
        return Some(SessionGoal {
            target_id: String::new(),
            objective: active.condition.clone(),
            title: String::new(),
            status: "active".to_owned(),
            state: GoalState::Working,
            round,
            time_used_seconds: now_ms.saturating_sub(active.set_at_ms) / 1000,
            running_since: 0,
            rounds: rounds(round, &active.condition, GoalVerdict::None),
            reason: active.last_reason.clone().unwrap_or_default(),
        });
    }
    let done = view.achieved.as_ref()?;
    let round = done.iterations.unwrap_or(1).max(1);
    let (state, status, last) = if done.failed {
        (GoalState::Failed, "failed", GoalVerdict::Failed)
    } else if done.met {
        (GoalState::Met, "verified", GoalVerdict::Met)
    } else {
        (GoalState::Stalled, "notSatisfied", GoalVerdict::Stalled)
    };
    Some(SessionGoal {
        target_id: String::new(),
        objective: done.condition.clone(),
        title: String::new(),
        status: status.to_owned(),
        state,
        round,
        time_used_seconds: done.duration_ms.unwrap_or(0) / 1000,
        running_since: 0,
        rounds: rounds(round, &done.condition, last),
        reason: done.reason.clone().unwrap_or_default(),
    })
}

impl App {
    /// 从会话取到的目标：记作会话的目标（状态栏），卡开着时换上；`opening`（`/goal` 刚发出）时打开卡，取的时候卡已经关上
    /// （打了字、开了别的）就不开。
    pub fn open_goal_card(&mut self, view: &GoalView, now_ms: u64, opening: bool) {
        self.show_goal_card(goal_of(view, now_ms), opening);
    }

    /// 会话的目标是 `goal`（[`Self::open_goal_card`] 的后半；孪生门禁直接交 kit 的目标）。
    pub fn show_goal_card(&mut self, goal: Option<SessionGoal>, opening: bool) {
        self.session_goal = goal.clone();
        match &mut self.overlay {
            Some(Overlay::Card(Card::Goal(shown))) => *shown = goal,
            None if opening && self.composer.is_empty() => {
                self.overlay = Some(Overlay::Card(Card::Goal(goal)));
            }
            _ => {}
        }
    }

    /// 发送：达成的目标的字从状态栏消失（Puffin 发送时的 `goal_met_seen`）。
    pub(super) fn goal_sent(&mut self) {
        self.goal_met_seen = self
            .session_goal
            .as_ref()
            .filter(|g| g.state == GoalState::Met)
            .map(kit_goal::met_key);
    }

    /// 状态栏模式后面目标的记号与字（kit `goal::status_parts`，Puffin `status_line.rs`）；没有目标时为 None。
    pub fn goal_status_parts(&self) -> Option<(Parts, Parts)> {
        let goal = self.session_goal.as_ref()?;
        let seen = self.goal_met_seen.as_deref() == Some(kit_goal::met_key(goal).as_str());
        Some(kit_goal::status_parts(&self.ink.theme, goal, seen))
    }

    /// 卡在淡入：按动画节拍重画。`now` 是画出的那一帧的时刻（tui.md §4.1）。
    pub fn cards_fading(&self, now: Instant) -> bool {
        if self.usage_moving(now) || self.usage_back_moving() {
            return true;
        }
        match &self.overlay {
            // 目标的卡开着、目标正在跑的这一段有开始时刻：用时每帧在走（Puffin `goal_moving` 的 `running_since > 0`；
            // 两边的 host 现在都给 0，内核给出这一段的开始时刻时才动）。
            Some(Overlay::Card(Card::Goal(Some(goal)))) => goal.running_since > 0,
            _ => false,
        }
    }

    /// 卡拿的键（Puffin `goal_key`、`stats_key`）：esc 关上；目标的卡 ⏎ 暂停或继续（`/goal pause` / `/goal resume`
    /// 发给内核，同 Puffin host 的 `PauseGoal` / `ResumeGoal`）；其余的键去编辑草稿，打了字卡就关上（[`Self::settle_card`]）。
    pub(super) fn card_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let Some(Overlay::Card(card)) = &self.overlay else {
            return None;
        };
        match *card {
            Card::Usage { entry } => return self.usage_card_key(key, entry),
            Card::UsagePage { entry, range, top } => {
                return self.usage_page_key(key, (entry, range, top));
            }
            Card::Goal(_) => {}
        }
        match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                Some(None)
            }
            KeyCode::Enter => match card {
                Card::Goal(goal) => {
                    Some(
                        kit_goal::enter_does(goal.as_ref()).map(|does| Action::Submit {
                            text: format!("/goal {does}"),
                            delivery: rust_cli_kernel_api::session_api::Delivery::Queue,
                        }),
                    )
                }
                Card::Usage { .. } | Card::UsagePage { .. } => Some(None),
            },
            _ => None,
        }
    }

    /// 一个键之后：草稿里有字了，卡关上（Puffin `settle_layer_card`）。
    pub(super) fn settle_card(&mut self) {
        if matches!(self.overlay, Some(Overlay::Card(_))) && !self.composer.is_empty() {
            self.overlay = None;
        }
    }

    /// 卡的行与槽（`pane.rs` 放在板上）；有请求在等时不画（Puffin `layer_card_open` 的 `!asking`）。
    /// `tick` 是这一帧的动画节拍（`/usage` 的卡按它淡入，usage_plans.rs）。
    pub fn card_layer(
        &self,
        width: usize,
        tick: i64,
        now_ms: u64,
    ) -> Option<(Vec<StyledLine>, Vec<Span>)> {
        let Some(Overlay::Card(card)) = &self.overlay else {
            return None;
        };
        if self.asking() {
            return None;
        }
        let theme: &Theme = &self.ink.theme;
        let (rows, keys) = match card {
            Card::Goal(goal) => (
                kit_goal::card(theme, goal.as_ref(), now_ms as f64),
                kit_goal::card_keys(goal.as_ref()),
            ),
            Card::Usage { entry } => {
                return Some(self.usage_card_layer(*entry, width, tick, now_ms));
            }
            // 详情页不在板上：它代替正文（`page_frame.rs`）。
            Card::UsagePage { .. } => return None,
        };
        Some((
            Layout::lay(&stack(rows), width),
            key_hints::spans(theme, &key_hints::pick(&keys)),
        ))
    }
}

#[cfg(test)]
#[path = "cards_tests.rs"]
mod tests;
