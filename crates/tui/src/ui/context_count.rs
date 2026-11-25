//! 上下文的计数（docs/specs/rust-cli/tui-panels.md §4.1、§4.3）：一次 `Kernel::context_usage` 的答案是设置的 Context、Details
//! 与 Status 的窗口一行唯一的数据源（[`ContextCount`]）。这几页显示时 runner 问（`runner/services.rs` 的 `count_context`）；答案
//! 带着问的会话，回来时看的已经不是它就丢掉、再问一次。Details 的开合与选中在设置的界面状态里（`Settings.context`），按路径记。
//!
//! 只问主会话：内核的 `context_usage` 按 ID 打开会话（`open_session` 冷加载），子代理的子会话由 agents 驱动，
//! 再开一个 actor 就有两个 owner。查看子代理时不问，Context 只说一句（§4.1）。
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rust_cli_kernel_api::context_usage::ContextUsage;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::markdown::render::SETTLED;
use rust_cli_tui_kit::render::StyledLine;

use super::app::App;
use super::context_page::{self, Inputs};
use super::context_tree::{self, Path};
use super::settings::live::Live;

/// 最近一次的答案（与它问的会话）、失败的原因与页上每个数变了的时刻。
#[derive(Debug, Default)]
pub struct ContextCount {
    /// 问的会话（None 是还没有会话：按新会话的模型与模式算）。
    answer: Option<(Option<SessionId>, ContextUsage)>,
    /// 最近一次失败（答案还没有时页上代替 `Counting…`）。
    failed: Option<String>,
    /// 页上的数变了的那一帧（键同 `context_page::figures`）：淡入从那一刻起。
    changed: BTreeMap<String, (u64, Instant)>,
}

/// Details 的大纲：开着的行与选中的行（路径）。设置打开时全部合着、选第一行。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextPage {
    pub open: BTreeSet<Path>,
    pub chosen: Path,
}

impl App {
    /// 主会话（还没有会话时 None）。
    fn context_session(&self) -> Option<SessionId> {
        self.session.as_deref().map(SessionId::new)
    }

    /// 要不要问：设置显示 Status、Context 或 Details、看的是主会话时问它（外层 None 是不问；内层 None 是还没有会话）。
    pub fn context_wanted(&self) -> Option<Option<SessionId>> {
        let shown = matches!(
            self.settings_live(),
            Some(Live::Status | Live::Context | Live::Details)
        );
        if self.viewing.is_some() || !shown {
            return None;
        }
        Some(self.context_session())
    }

    /// 现在看的会话的答案（查看子代理时没有）。
    pub fn context_now(&self) -> Option<&ContextUsage> {
        let (session, usage) = self.context_count.answer.as_ref()?;
        (self.viewing.is_none() && *session == self.context_session()).then_some(usage)
    }

    /// 答案回来。问的不是现在看的会话时丢掉，返回 true（runner 按现在的再问一次）。数变了的记下时刻（第一次的答案不淡）。
    pub fn context_counted(
        &mut self,
        session: Option<SessionId>,
        result: Result<ContextUsage, String>,
        now: Instant,
        now_ms: u64,
    ) -> bool {
        if self.viewing.is_some() || session != self.context_session() {
            return true;
        }
        match result {
            Ok(usage) => {
                if let Some(before) = self.context_now() {
                    let before: BTreeMap<String, u64> =
                        context_page::figures(before).into_iter().collect();
                    for (key, value) in context_page::figures(&usage) {
                        if before.get(&key) != Some(&value) {
                            self.context_count.changed.insert(key, (now_ms, now));
                        }
                    }
                }
                self.context_count.answer = Some((session, usage));
                self.context_count.failed = None;
            }
            Err(reason) => self.context_count.failed = Some(reason),
        }
        false
    }

    /// 页上的数在淡入。`now` 是画出的那一帧的时刻（tui.md §4.1）。
    pub(super) fn context_fading(&self, now: Instant) -> bool {
        let ticks = u128::from(TICK_MS) * u128::from(fade::TICKS.unsigned_abs());
        self.context_count
            .changed
            .values()
            .any(|(_, at)| now.saturating_duration_since(*at).as_millis() < ticks)
    }

    /// 规则文件的路径相对于工作区、家目录写成 `~`。
    fn context_places(&self) -> context_tree::Places<'_> {
        context_tree::Places {
            root: self.workspace(),
            home: self.home.as_deref(),
        }
    }

    /// Context 与 Details 的入参：答案、失败的原因、开合与选中、每个数变了多少 tick。
    fn context_inputs<'a>(
        &'a self,
        page: &'a ContextPage,
        width: usize,
        age: &'a dyn Fn(&str) -> i64,
    ) -> Inputs<'a> {
        Inputs {
            theme: &self.ink.theme,
            lang: self.lang,
            width,
            usage: self.context_now(),
            failed: self.context_count.failed.as_deref(),
            open: &page.open,
            chosen: &page.chosen,
            age,
            places: self.context_places(),
        }
    }

    /// 那个数变了多少 tick（键：`legend/组` 或路径用 `/` 连起来）：第一次的答案与没变的落定。
    fn context_age(&self, now_ms: u64) -> impl Fn(&str) -> i64 + '_ {
        move |key: &str| {
            self.context_count
                .changed
                .get(key)
                .map_or(SETTLED, |(at, _)| {
                    i64::try_from(now_ms.saturating_sub(*at) / TICK_MS).unwrap_or(SETTLED)
                })
        }
    }

    /// Context 段的行（§4.2）。
    pub(super) fn context_overview(&self, width: usize, now_ms: u64) -> Vec<StyledLine> {
        let page = ContextPage::default();
        let age = self.context_age(now_ms);
        context_page::overview(&self.context_inputs(&page, width, &age))
    }

    /// Details 的行与选中的行在第几行（§4.3）。
    pub(super) fn context_details(
        &self,
        page: &ContextPage,
        width: usize,
        now_ms: u64,
    ) -> context_page::Page {
        let age = self.context_age(now_ms);
        context_page::details(&self.context_inputs(page, width, &age))
    }

    /// Details 的大纲（答案还没到时没有）。
    pub(super) fn context_outline(&self) -> Option<Vec<context_tree::Node>> {
        self.context_now()
            .map(|usage| context_tree::outline(self.lang, usage, &self.context_places()))
    }
}

#[cfg(test)]
#[path = "context_count_tests.rs"]
mod tests;
