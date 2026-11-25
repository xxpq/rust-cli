//! 回合里的提交失败（docs/specs/rust-cli/usage.md §2.5、logging.md §4，工作线 F77）：回合以存储错误结束，同时记
//! 一条带原因的 warn。以前错误只进回合的结局，日志里没有一行。
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, Once};

use rust_cli_kernel_api::session_api::{KernelEvent, SessionHandle, TurnOutcome};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tracing::field::Field;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::test_support::{Setup, finished, submit, until};

/// 一条 warn 事件：字段名 → 值（`Debug` 的文字）。
type Fields = BTreeMap<&'static str, String>;

thread_local! {
    /// 当前线程从 [`collect_warnings`] 起收到的 warn；不在收集时为空。
    static COLLECTED: RefCell<Option<Vec<Fields>>> = const { RefCell::new(None) };
}

/// 从现在起收集当前线程上的 warn（[`take_warnings`] 取走）。单线程运行时里会话 actor 与回合任务都在这个线程上。
///
/// 修 bug（FLAKES，usage.md §9）：以前用 `set_default` 装只在本线程的订阅者。tracing 的回调点兴趣是全局缓存，进程里
/// 只登记了一个订阅者时，回调点第一次命中时的兴趣按命中线程的缺省订阅者算；别的测试（`hooks_tests.rs` 的提交失败）在
/// 自己的线程上先命中同一个 `session.write.failed`，兴趣缓存成 never，本线程的订阅者收不到（固定这个先后时 3 次全丢）。
/// 现在装测试进程的全局订阅者（只装一次），它在每个线程上都是缺省的；事件按线程交给正在收集的一方。
fn collect_warnings() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(ThreadWarnings).unwrap();
        // 装的过程中（订阅者已登记、全局缺省还没设上）别的线程登记的回调点按“没有订阅者”缓存成了 never：重算一次。
        tracing::callsite::rebuild_interest_cache();
    });
    COLLECTED.with_borrow_mut(|collected| *collected = Some(Vec::new()));
}

/// 取走 [`collect_warnings`] 以来当前线程上的 warn，并停止收集。
fn take_warnings() -> Vec<Fields> {
    COLLECTED.with_borrow_mut(Option::take).unwrap_or_default()
}

/// 测试进程的全局订阅者：warn 事件交给命中线程正在收集的 [`COLLECTED`]，别的丢掉。
struct ThreadWarnings;

impl Subscriber for ThreadWarnings {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() == Level::WARN
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        COLLECTED.with_borrow_mut(|collected| {
            let Some(collected) = collected else {
                return;
            };
            let mut fields = BTreeMap::new();
            event.record(&mut |field: &Field, value: &dyn Debug| {
                fields.insert(field.name(), format!("{value:?}"));
            });
            collected.push(fields);
        });
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[tokio::test]
async fn a_failed_write_ends_the_turn_and_is_logged() {
    // 单线程运行时：actor 与回合任务都在这个线程上，按线程收集看得到它们的事件。
    collect_warnings();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["hi"])])));
    // 存储里没有这个会话：每次提交都失败。
    setup.stored = false;
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    let Some(KernelEvent::TurnFinished {
        outcome: TurnOutcome::Failed { message, .. },
        ..
    }) = seen.last()
    else {
        panic!("{seen:?}");
    };
    let logged: Vec<_> = take_warnings()
        .into_iter()
        .filter(|fields| {
            fields.get("event").map(String::as_str) == Some("\"session.write.failed\"")
        })
        .collect();
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0]["session"], "sess_1");
    assert_eq!(&logged[0]["reason"], message);
}
