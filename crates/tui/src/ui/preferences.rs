//! 偏好写不成时说的一句（docs/specs/rust-cli/tui-kit.md §4.5「偏好写不成时说一句」，用户决定 2026-11-12）：主题
//! （`theme.json`）、任务列表的分组与 ← 打开任务列表的开关（`cockpit.json`）都先当场生效、再写文件；写失败时三处都经这里，
//! 在输入框上方安静的一行（设置开着时是页的那一行）说没能保存、只在这次运行有效。以前主题只在设置页还开着时说、`/theme`
//! 另有一句，任务列表的两样什么都不说。
use super::app::App;
use super::i18n::Lang;

/// 哪一样偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pref {
    Theme,
    Grouping,
    LeftArrow,
}

/// 没能保存 `pref` 的一句（原因是存储给的原文，不译）。
pub fn unsaved(lang: Lang, pref: Pref, reason: &str) -> String {
    match lang {
        Lang::En => {
            let what = match pref {
                Pref::Theme => "the theme",
                Pref::Grouping => "the task list grouping",
                Pref::LeftArrow => "the ← opens the task list setting",
            };
            format!("Couldn't save {what}, so it holds for this run only: {reason}")
        }
        Lang::Zh => {
            let what = match pref {
                Pref::Theme => "主题",
                Pref::Grouping => "任务列表的分组",
                Pref::LeftArrow => "“← 打开任务列表”的开关",
            };
            format!("没能保存{what}，只在这次运行有效：{reason}")
        }
    }
}

impl App {
    /// 偏好写不成：输入框上方安静的一行（[`App::info`]：设置开着时是页的那一行）。
    pub fn preference_unsaved(&mut self, pref: Pref, reason: &str) {
        let said = unsaved(self.lang, pref, reason);
        self.info(said);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_preferences_share_one_sentence() {
        assert_eq!(
            unsaved(Lang::En, Pref::Grouping, "disk full"),
            "Couldn't save the task list grouping, so it holds for this run only: disk full"
        );
        assert_eq!(
            unsaved(Lang::Zh, Pref::Theme, "disk full"),
            "没能保存主题，只在这次运行有效：disk full"
        );
        assert_eq!(
            unsaved(Lang::Zh, Pref::LeftArrow, "x"),
            "没能保存“← 打开任务列表”的开关，只在这次运行有效：x"
        );
    }
}
