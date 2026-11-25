//! 访问钥匙串之前的提示（platform.md §13.12）：只提示一次；启动画面期间记下的待提示在界面开始时发出。

use futures_util::FutureExt;
use rust_cli_tui::KeychainNotices;

use super::*;

/// 界面马上能收到的信号个数（不等）。
fn ready(stream: &mut BoxStream<'static, ()>) -> usize {
    let mut count = 0;
    while let Some(Some(())) = stream.next().now_or_never() {
        count += 1;
    }
    count
}

#[test]
fn an_access_during_the_screens_is_shown_when_the_ui_starts() {
    let notice = KeychainNotice::default();
    notice.terminal("text");
    notice.hold();
    notice.before_access();
    notice.before_access();
    let mut stream = notice.notices();
    assert_eq!(ready(&mut stream), 1);
    notice.before_access();
    assert_eq!(ready(&mut stream), 0);
}

#[test]
fn an_access_while_the_ui_runs_sends_one_signal() {
    let notice = KeychainNotice::default();
    notice.terminal("text");
    notice.hold();
    let mut stream = notice.notices();
    assert_eq!(ready(&mut stream), 0);
    notice.before_access();
    notice.before_access();
    assert_eq!(ready(&mut stream), 1);
}

/// 启动画面之前的那一次写在终端里（这里是测试的 stderr），界面开始后不再提示。
#[test]
fn the_startup_access_is_written_once_to_the_terminal() {
    let notice = KeychainNotice::default();
    notice.terminal("text");
    notice.before_access();
    assert!(matches!(
        &*notice.state.lock_unpoisoned(),
        NoticeState::Shown
    ));
    notice.hold();
    let mut stream = notice.notices();
    notice.before_access();
    assert_eq!(ready(&mut stream), 0);
}

/// 没有访问钥匙串（v1 文件、受管密钥）时什么都不提示。
#[test]
fn no_access_no_notice() {
    let notice = KeychainNotice::default();
    notice.terminal("text");
    notice.hold();
    let mut stream = notice.notices();
    assert_eq!(ready(&mut stream), 0);
    assert!(matches!(
        &*notice.state.lock_unpoisoned(),
        NoticeState::Ui(_)
    ));
}
