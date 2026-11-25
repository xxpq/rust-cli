// 复制自 apps/zcode-host/crates/tui/src/ui/notice.rs（feat/rs 79b6691c55）。
//! footer 的临时提示与连按两次的按键（docs/specs/rust-tui.md §8.4、§8.6）。提示到时自动消失；连按的
//! 判断只看这条提示：同一种按键在它显示期间（800 ms）再按一次就生效。
use std::time::{Duration, Instant};

use super::app::App;
use super::i18n::Lang;

/// 连按两次的时限。
pub const DOUBLE: Duration = Duration::from_millis(800);
pub const HINT_FOR: Duration = Duration::from_secs(5);

/// 等待第二次按下的按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Armed {
    /// ctrl+c：再按一次退出。
    ExitC,
    /// ctrl+d：再按一次退出。
    ExitD,
    /// esc（输入框为空）：再按一次在最后一条消息上挑选回退的点。
    Edit,
    /// ←（刚删空草稿时）：再按一次打开任务列表（tui-cockpit.md §3.5）。
    Cockpit,
}

impl Armed {
    /// 等第二次按下多久：← 是 3 s（tui-cockpit.md §3.5），其余是 [`DOUBLE`]。
    fn window(self) -> Duration {
        match self {
            Armed::Cockpit => super::cockpit_gesture::ARMED,
            Armed::ExitC | Armed::ExitD | Armed::Edit => DOUBLE,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub text: String,
    pub until: Instant,
    pub armed: Option<Armed>,
    /// `text` 是按键提示模板（`{键}` 标出按键，tui.md §7.4）；其余提示里可能有任意文字，按原文显示。
    pub keyed: bool,
}

pub(crate) fn armed_text(lang: Lang, armed: Armed) -> &'static str {
    match (lang, armed) {
        (Lang::En, Armed::ExitC) => "Press {^C} again to exit",
        (Lang::En, Armed::ExitD) => "Press {^D} again to exit",
        (Lang::En, Armed::Edit) => "{esc} rewind",
        (Lang::En, Armed::Cockpit) => "Press {←} again for the agent cockpit",
        (Lang::Zh, Armed::ExitC) => "再按一次 {^C} 退出",
        (Lang::Zh, Armed::ExitD) => "再按一次 {^D} 退出",
        (Lang::Zh, Armed::Edit) => "{esc} 回退",
        (Lang::Zh, Armed::Cockpit) => "再按一次 {←} 打开任务列表",
    }
}

/// 本进程第一次访问 macOS 钥匙串之前的提示（用户决定 2026-11-11，platform.md §13.12、tui.md §7.10）：系统可能弹框问用户。
pub fn keychain(lang: Lang) -> &'static str {
    lang.pick(
        "Reading the login key from the keychain; macOS may ask.",
        "要读取钥匙串里的登录密钥，系统可能会弹框询问。",
    )
}

/// 钥匙串的提示在 footer 留多久：系统弹框可能要用户想一会儿，比按键提示（[`HINT_FOR`]）长。
pub const KEYCHAIN_FOR: Duration = Duration::from_secs(10);

pub fn search_history(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "{^R} to search history",
        Lang::Zh => "{^R} 搜索历史",
    }
}

impl App {
    /// 显示一条临时提示，`duration` 后消失。
    pub fn notify(&mut self, text: impl Into<String>, duration: Duration) {
        self.notice = Some(Notice {
            text: text.into(),
            until: Instant::now() + duration,
            armed: None,
            keyed: false,
        });
    }

    /// 显示一条按键提示（模板里的 `{键}` 按按键样式画），`duration` 后消失。
    pub fn notify_keys(&mut self, template: &str, duration: Duration) {
        self.notify(template, duration);
        if let Some(notice) = self.notice.as_mut() {
            notice.keyed = true;
        }
    }

    /// 连按两次的按键：提示期间再按一次返回 true（并清掉提示）；否则显示提示并返回 false。
    pub(super) fn second_press(&mut self, armed: Armed, now: Instant) -> bool {
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.armed == Some(armed) && now < n.until)
        {
            self.notice = None;
            return true;
        }
        self.notice = Some(Notice {
            text: armed_text(self.lang, armed).to_owned(),
            until: now + armed.window(),
            armed: Some(armed),
            keyed: true,
        });
        false
    }

    /// `now` 时还在显示的提示（按键提示模板去掉标记后的文字）。
    pub fn notice_text(&self, now: Instant) -> Option<String> {
        self.live_notice(now).map(|n| {
            if n.keyed {
                super::key_hint::plain(&n.text)
            } else {
                n.text.clone()
            }
        })
    }

    /// `now` 时还在显示的提示本身。画帧时给画的时刻；判断要不要再画时给画出的那一帧的时刻（tui.md §4.1）。
    ///
    /// 修 bug（FLAKES）：以前总读 `Instant::now()`。提示在“画帧”与“判断”之间到时，判断说已经消失、不再重画，槽停在
    /// 这条提示（如 `esc rewind`），直到下一次输入或事件。
    pub fn live_notice(&self, now: Instant) -> Option<&Notice> {
        self.notice.as_ref().filter(|n| now < n.until)
    }
}
