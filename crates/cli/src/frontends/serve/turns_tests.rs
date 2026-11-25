//! 轮次表：输出缓冲的 seq 与截断、连接计数、宽限到时的判断。

use super::*;

fn key() -> TurnKey {
    TurnKey {
        user: "u".to_owned(),
        turn_id: "t".to_owned(),
    }
}

#[test]
fn a_turn_key_is_registered_once() {
    let turns = Turns::default();
    assert!(turns.begin(key()).is_some());
    assert!(turns.begin(key()).is_none());
    assert!(turns.find(&key()).is_some());
    turns.end(&key());
    assert_eq!(turns.len(), 0);
}

#[test]
fn lines_get_consecutive_seqs_and_resume_after_one() {
    let log = TurnLog::new(key());
    for text in ["a", "b", "c"] {
        log.push(|seq| format!("{text}{seq}"));
    }
    assert_eq!(
        log.since(1),
        Since::Lines(vec![(2, "b2".to_owned()), (3, "c3".to_owned())])
    );
    assert_eq!(log.since(3), Since::Lines(Vec::new()));
    assert_eq!(
        *log.watch().borrow(),
        Progress {
            last: 3,
            finished: false
        }
    );
    assert!(!log.finish());
    assert!(log.watch().borrow().finished);
}

#[test]
fn the_buffer_drops_the_oldest_lines_beyond_its_limit() {
    let log = TurnLog::new(key());
    let big = "x".repeat(MAX_BUFFER_BYTES / 2 + 1);
    log.push(|_| big.clone());
    log.push(|_| big.clone());
    log.push(|_| "tail".to_owned());
    // 第一行丢了：从 0 之后要不到，从 1 之后还能接上。
    assert_eq!(log.since(0), Since::Truncated);
    assert!(matches!(log.since(1), Since::Lines(lines) if lines.len() == 2));
}

#[test]
fn a_grace_expires_only_when_nobody_reattached() {
    let log = TurnLog::new(key());
    log.attach();
    let generation = log.detach().unwrap();
    // 宽限内有连接续上：到时不打断。
    log.attach();
    assert_eq!(log.expire(generation), Expiry::Kept);
    let generation = log.detach().unwrap();
    assert_eq!(log.expire(generation), Expiry::Interrupt);
    log.interrupt();
    assert!(log.cancel.is_cancelled() && log.stop.is_cancelled());
    // 过期之后跑完：没人会取，调用方拿掉。
    assert!(log.finish());
}

#[test]
fn a_finished_turn_without_a_connection_is_dropped_at_expiry() {
    let log = TurnLog::new(key());
    log.attach();
    log.push(|seq| format!("line{seq}"));
    assert!(!log.finish());
    let generation = log.detach().unwrap();
    assert_eq!(log.expire(generation), Expiry::Drop);
}

#[test]
fn two_connections_keep_the_turn_attached() {
    let log = TurnLog::new(key());
    log.attach();
    log.attach();
    assert_eq!(log.detach(), None);
    assert!(log.detach().is_some());
}
