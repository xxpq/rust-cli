//! 终端按键的入口归一（docs/specs/rust-cli/tui-input.md §9.1，修复线 F65；架构整理 U9）。
//!
//! - [`normalize`]：Windows 把 AltGr 报成 Ctrl+Alt，crossterm 照样给出 Ctrl+Alt 加产生的字符：Windows 上它是文字。入口
//!   去掉这两个修饰键，之后处理按键的地方只看普通修饰键（[`ctrl`]、[`alt`]、[`is_text`]、[`is_plain_text`]）。
//! - [`Keyboard`]：主界面的入口：松开事件不进界面，再归一。Windows 的 alt+小键盘码由 vendor 的 crossterm 报成按下
//!   （tui-input.md §9.1，修复线 F148）。
//!
//! 架构整理 U9：以前按平台的判断 `Typing` 由 `App` 持有、经参数传给列表、询问框与各面板，另有 10 处直接读 `CONTROL`，
//! 两种读法在不经 paste burst 的入口上不一致。现在只在入口判断一次。
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::os::Os;

/// Windows 的 AltGr（Ctrl+Alt 加一个可打印字符）去掉 Ctrl 与 Alt；其他按键与其他平台原样返回。
pub fn normalize(key: KeyEvent, os: Os) -> KeyEvent {
    let altgr = key
        .modifiers
        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char(c) if !c.is_control());
    if os.is_windows() && altgr {
        KeyEvent {
            modifiers: key.modifiers - (KeyModifiers::CONTROL | KeyModifiers::ALT),
            ..key
        }
    } else {
        key
    }
}

/// 快捷键看的 ctrl（入口归一之后，AltGr 不带它）。
pub fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

/// 快捷键看的 alt。
pub fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::ALT)
}

/// 文字输入：字符键且没有 ctrl（alt+字符也算，输入框等原来只排除 ctrl 的地方）。
pub fn is_text(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_)) && !ctrl(key)
}

/// 文字输入，并且没有 alt（`/resume`、项目页的输入，以及 paste burst 的判定）。
pub fn is_plain_text(key: &KeyEvent) -> bool {
    is_text(key) && !alt(key)
}

/// 按词删除的方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordKill {
    /// 删光标前的词。
    Left,
    /// 删光标后的词。
    Right,
}

/// 按词删除的键（tui-input.md §5）：ctrl+w、ctrl / alt 加 ⌫ 删前一个词；alt+d、ctrl / alt 加 delete 删后一个词。所有文字
/// 输入都经这里判断，键位一致。
///
/// 以前 ctrl / alt 加 ⌫、delete 在输入框里只删一个字符（各处的 ⌫ 分支不看修饰键），单行输入没有按词删除。ctrl+h 不算：
/// 传统终端里它可能就是普通的 ⌫（0x08），当作删词会让 ⌫ 一次删一个词（用户决定 2026-11-07）。
pub fn word_kill(key: &KeyEvent) -> Option<WordKill> {
    let (ctrl, alt) = (ctrl(key), alt(key));
    match key.code {
        KeyCode::Backspace if ctrl || alt => Some(WordKill::Left),
        KeyCode::Char('w') if ctrl => Some(WordKill::Left),
        KeyCode::Delete if ctrl || alt => Some(WordKill::Right),
        // ctrl+alt+d 不算：输入框里 ctrl+d 先到（删一个字符或退出）。
        KeyCode::Char('d') if alt && !ctrl => Some(WordKill::Right),
        _ => None,
    }
}

/// 按词移动（alt+← / →、ctrl+← / →）：返回是不是往右。alt+b / alt+f 只在输入框里（单行输入里 alt+字符是文字）。
pub fn word_move(key: &KeyEvent) -> Option<bool> {
    match key.code {
        KeyCode::Left | KeyCode::Right if ctrl(key) || alt(key) => Some(key.code == KeyCode::Right),
        _ => None,
    }
}

/// 主界面的按键入口：丢掉松开事件，AltGr 归一。
///
/// 缺陷修复（修复线 F148，tui-input.md §9.1）：以前 crossterm 把 Windows 的 alt 码报成 Alt 键的松开，这里只能推断——记下
/// 按下了、还没松开的字符键，没有配对的带字符的松开当作 alt 码。输入法吃掉选字那个键（空格、数字、中文标点的键）的按下、
/// 只把松开放过来（conhost GH#20244），于是用空格选字后多打一个空格。现在 vendor 的 crossterm 按虚拟键码认出 alt 码、报成
/// 按下，这里松开一律不要（同 libuv），配对与焦点之后的规则一起删掉。
#[derive(Debug, Clone, Default)]
pub struct Keyboard {
    os: Os,
}

impl Keyboard {
    pub fn new(os: Os) -> Self {
        Self { os }
    }

    /// 进入界面的按键：按下与重复照常，AltGr 归一（[`normalize`]）；松开丢掉。
    pub fn admit(&self, key: KeyEvent) -> Option<KeyEvent> {
        (key.kind != KeyEventKind::Release).then(|| normalize(key, self.os))
    }
}

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
