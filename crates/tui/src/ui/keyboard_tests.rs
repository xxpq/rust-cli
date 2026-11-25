use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{
    Keyboard, WordKill, alt, ctrl, is_plain_text, is_text, normalize, word_kill, word_move,
};
use crate::os::{Os, WindowsRelease};

const WINDOWS: Os = Os::Windows(Some(WindowsRelease {
    major: 10,
    build: 22631,
}));
const CTRL_ALT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn up(c: char, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new_with_kind(KeyCode::Char(c), modifiers, KeyEventKind::Release)
}

/// T2：Windows 把 AltGr 报成 Ctrl+Alt（德语键盘的 `@`、`{`，波兰语的 `ł`）。Windows 上入口把它们归一成文字，ctrl 与
/// alt 都不算；其他平台照旧是 ctrl 组合。
#[test]
fn altgr_characters_are_text_only_on_windows() {
    for c in ['@', '{', '\\', '€', 'ł'] {
        let altgr = key(KeyCode::Char(c), CTRL_ALT);
        let windows = normalize(altgr, WINDOWS);
        assert!(is_text(&windows), "{c}");
        assert!(is_plain_text(&windows), "{c}");
        assert!(!ctrl(&windows) && !alt(&windows), "{c}");
        let unix = normalize(altgr, Os::Unix);
        assert!(!is_text(&unix), "{c}");
        assert!(ctrl(&unix) && alt(&unix), "{c}");
    }
    // 读不到系统版本也是 Windows；shift 留着。
    let shifted = key(KeyCode::Char('@'), CTRL_ALT | KeyModifiers::SHIFT);
    assert_eq!(
        normalize(shifted, Os::Windows(None)).modifiers,
        KeyModifiers::SHIFT
    );
}

/// 只有 Ctrl+Alt 加可打印字符算 AltGr：单独的 ctrl、单独的 alt、Ctrl+Alt 加方向键照旧。
#[test]
fn other_modifiers_keep_their_meaning_on_windows() {
    let ctrl_c = normalize(key(KeyCode::Char('c'), KeyModifiers::CONTROL), WINDOWS);
    assert!(!is_text(&ctrl_c) && ctrl(&ctrl_c));
    let alt_b = normalize(key(KeyCode::Char('b'), KeyModifiers::ALT), WINDOWS);
    assert!(is_text(&alt_b) && !is_plain_text(&alt_b));
    assert!(alt(&alt_b));
    let left = normalize(key(KeyCode::Left, CTRL_ALT), WINDOWS);
    assert!(!is_text(&left));
    assert!(ctrl(&left) && alt(&left));
    let shifted = normalize(key(KeyCode::Char('A'), KeyModifiers::SHIFT), WINDOWS);
    assert!(is_plain_text(&shifted));
    let control = normalize(key(KeyCode::Char('\u{1}'), CTRL_ALT), WINDOWS);
    assert!(!is_text(&control));
}

/// 主界面的入口归一：AltGr 的按下去掉 Ctrl+Alt，松开丢掉。
#[test]
fn the_entry_normalizes_altgr() {
    let keyboard = Keyboard::new(WINDOWS);
    assert_eq!(
        keyboard.admit(key(KeyCode::Char('@'), CTRL_ALT)),
        Some(key(KeyCode::Char('@'), KeyModifiers::NONE))
    );
    assert_eq!(keyboard.admit(up('@', CTRL_ALT)), None);
    let unix = Keyboard::new(Os::Unix);
    let altgr = key(KeyCode::Char('@'), CTRL_ALT);
    assert_eq!(unix.admit(altgr), Some(altgr));
}

/// T3（修复线 F148，tui-input.md §9.1）：输入法用空格选字时吃掉空格的按下，只把松开放过来（conhost GH#20244）；数字选字、
/// 中文标点同样。入口丢掉所有松开，只留选中的文字。修复前没有配对的松开当作 alt 码，打出的是 `你好 ，,`。
#[test]
fn releases_the_ime_passes_on_are_dropped() {
    let keyboard = Keyboard::new(WINDOWS);
    let mut typed = String::new();
    let mut feed = |event: KeyEvent| {
        if let Some(KeyEvent {
            code: KeyCode::Char(c),
            ..
        }) = keyboard.admit(event)
        {
            typed.push(c);
        }
    };
    feed(key(KeyCode::Char('n'), KeyModifiers::NONE));
    feed(up('n', KeyModifiers::NONE));
    for c in ['你', '好'] {
        feed(key(KeyCode::Char(c), KeyModifiers::NONE));
        feed(up(c, KeyModifiers::NONE));
    }
    feed(up(' ', KeyModifiers::NONE));
    feed(up('2', KeyModifiers::NONE));
    feed(key(KeyCode::Char('，'), KeyModifiers::NONE));
    feed(up('，', KeyModifiers::NONE));
    feed(up(',', KeyModifiers::NONE));
    assert_eq!(typed, "n你好，");
}

/// alt+小键盘码：vendor 的 crossterm 把 Alt 松开时带的字符报成按下（tui-input.md §9.1），入口照常收下；别的松开都丢掉，
/// 平台都一样。
#[test]
fn alt_codes_arrive_as_presses_and_releases_are_dropped() {
    for os in [WINDOWS, Os::Unix] {
        let keyboard = Keyboard::new(os);
        let alt_code = key(KeyCode::Char('é'), KeyModifiers::NONE);
        assert_eq!(keyboard.admit(alt_code), Some(alt_code));
        assert_eq!(keyboard.admit(up('ä', KeyModifiers::NONE)), None);
        assert_eq!(keyboard.admit(up('é', KeyModifiers::ALT)), None);
        let enter =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(keyboard.admit(enter), None);
        let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(keyboard.admit(a), Some(a));
    }
}

/// 按词删除与移动的键（tui-input.md §5）：所有文字输入共用这一处判断。ctrl+h 不算（传统终端里可能就是 ⌫）；ctrl+alt+d
/// 不算（输入框里 ctrl+d 先到）；不带修饰键的 ⌫、delete、←、→ 不算。
#[test]
fn word_keys_are_read_in_one_place() {
    let none = KeyModifiers::NONE;
    let (c, a) = (KeyModifiers::CONTROL, KeyModifiers::ALT);
    for left in [
        key(KeyCode::Backspace, c),
        key(KeyCode::Backspace, a),
        key(KeyCode::Char('w'), c),
    ] {
        assert_eq!(word_kill(&left), Some(WordKill::Left), "{left:?}");
    }
    for right in [
        key(KeyCode::Delete, c),
        key(KeyCode::Delete, a),
        key(KeyCode::Char('d'), a),
    ] {
        assert_eq!(word_kill(&right), Some(WordKill::Right), "{right:?}");
    }
    for not in [
        key(KeyCode::Backspace, none),
        key(KeyCode::Delete, none),
        key(KeyCode::Char('h'), c),
        key(KeyCode::Char('d'), c),
        key(KeyCode::Char('d'), CTRL_ALT),
        key(KeyCode::Char('w'), none),
        key(KeyCode::Char('w'), a),
    ] {
        assert_eq!(word_kill(&not), None, "{not:?}");
    }
    assert_eq!(word_move(&key(KeyCode::Left, c)), Some(false));
    assert_eq!(word_move(&key(KeyCode::Left, a)), Some(false));
    assert_eq!(word_move(&key(KeyCode::Right, c)), Some(true));
    assert_eq!(word_move(&key(KeyCode::Right, a)), Some(true));
    assert_eq!(word_move(&key(KeyCode::Left, none)), None);
    assert_eq!(word_move(&key(KeyCode::Char('b'), a)), None);
}
