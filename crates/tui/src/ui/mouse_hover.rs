//! 悬停（tui-select.md §9）：悬停要靠移动得来。鼠标移动时记下位置与当时屏幕上那里的东西；绘制时那里还是它才铺悬停。
//!
//! 缺陷修复：以前只存鼠标最后的位置，每一帧按位置找可点击区域与块。终端只在鼠标移动时报告位置，鼠标不动或离开窗口时
//! 存的位置过时了，后来画到那里的东西（被推过来的块、弹出的列表、审批、任务行）都被高亮，用户没碰鼠标也是。现在比较
//! 记下的东西，按键、粘贴与焦点变化清掉悬停。
use super::{Target, contains};
use crate::ui::app::App;
use crate::ui::geom::Rect;

/// 鼠标最后移到的位置，与移到那里时屏幕上那里的东西（没有时 `None`）：可点击的目标、链接（目标与它的块）或块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    at: (u16, u16),
    target: Option<Target>,
    link: Option<(String, u64)>,
    block: Option<u64>,
}

impl App {
    /// 鼠标移动：按上一帧（用户看到的那一帧）的 `Hits` 记下那里的目标或块。
    pub(super) fn hover_moved(&mut self, x: u16, y: u16) {
        self.pointer.hover = Some(Hover {
            at: (x, y),
            target: self.target_at(x, y),
            link: self.link_at(x, y).map(|link| (link.url.clone(), link.key)),
            block: self.block_at(x, y),
        });
    }

    /// 按键、粘贴、焦点变化：直到鼠标再动之前没有悬停。
    pub(crate) fn clear_hover(&mut self) {
        self.pointer.hover = None;
    }

    /// 这一帧铺悬停的可点击区域：鼠标所在处最上面的区域，目标与移到那里时记下的相同。在记下这一帧的全部区域之后调用。
    pub(super) fn hovered_region(&self) -> Option<Rect> {
        let hover = self.pointer.hover.as_ref()?;
        let (x, y) = hover.at;
        let (rect, target) = self
            .pointer
            .hits
            .regions
            .iter()
            .rev()
            .find(|(rect, _)| contains(*rect, x, y))?;
        (hover.target.as_ref() == Some(target)).then_some(*rect)
    }

    /// 悬停的链接（tui-select.md §10）：鼠标所在处的链接，与移到那里时记下的相同（同一个目标、同一个块）。
    pub(super) fn hovered_link(&self) -> Option<(String, u64)> {
        let hover = self.pointer.hover.as_ref()?;
        let (x, y) = hover.at;
        let link = self.link_at(x, y)?;
        let now = (link.url.clone(), link.key);
        (hover.link.as_ref() == Some(&now)).then_some(now)
    }

    /// 悬停的可展开块（§8.2、§9.1）：鼠标停在块的文字上，块与移到那里时记下的相同。收起的与展开的都算：展开的块点了会收起，
    /// 也是能点的。修改原因（2026-11-12，用户：“所有能鼠标点击的，hover 上去都需要一个交互”）：以前展开的块没有悬停效果。
    pub fn hovered_block(&self) -> Option<u64> {
        let hover = self.pointer.hover.as_ref()?;
        let (x, y) = hover.at;
        self.block_at(x, y).filter(|key| hover.block == Some(*key))
    }
}

#[cfg(test)]
#[path = "mouse_hover_tests.rs"]
mod tests;
