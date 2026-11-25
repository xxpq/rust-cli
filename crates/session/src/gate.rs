//! 准入的拒绝在会话 actor 这一侧的文字（session-lifecycle.md §10.4，K-S1）：原因由 kernel-api 的 `admission::admit` 给出（闭集），
//! 文字只给人看，按命令取以前各处理函数里的那一句（TUI 照旧显示它）。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::session_api::Command;

/// 正在关闭的会话。
pub(crate) const CLOSED: &str = "the session is closed";
/// 停止时没有回合在跑。
pub(crate) const NOT_RUNNING: &str = "no turn is running";
/// 指名停止的回合已经结束，在跑的是另一个（§10.5）。
const TARGET_CHANGED: &str = "that turn already ended; another turn is running";
/// 排队的输入扣住了（§10.6）。
const QUEUE_HELD: &str = "queued inputs are on hold; resume or take them back first";
const COMPACT_BUSY: &str = "a turn is running; compact after it finishes";
const FORK_BUSY: &str = "a turn is running; fork after it finishes";
/// v5 的 fork 时有压缩或会话的后台工作在跑（§10.57；回合在跑时它照常分）。
const FORK_AS_BUSY: &str = "a compaction or session job is running; fork after it finishes";
const REWIND_BUSY: &str = "a turn is running; rewind after it finishes";
/// 换掉一条输入时有压缩或会话的后台工作在跑（§10.53）。
const REPLACE_BUSY: &str = "a compaction or session job is running; edit after it finishes";
const RESTORE_BUSY: &str = "a turn is running";
/// 回合进行中改目录。
const DIRECTORY_BUSY: &str = "A turn is in progress — the working directory can only change while the session is idle. Wait for the turn to finish (or interrupt it), then retry.";

/// `admit` 拒绝一条命令时给人看的文字。
pub(crate) fn refused(command: &Command, refusal: &Refusal) -> &'static str {
    match (refusal, command) {
        (Refusal::Closed, _) => CLOSED,
        (Refusal::NotRunning, _) => NOT_RUNNING,
        (Refusal::StopTargetChanged { .. }, _) => TARGET_CHANGED,
        (Refusal::QueueChoiceRequired, _) => QUEUE_HELD,
        (_, Command::Compact { .. }) => COMPACT_BUSY,
        (_, Command::Fork { .. } | Command::ForkFromInput { .. }) => FORK_BUSY,
        (_, Command::ForkFromInputAs { .. }) => FORK_AS_BUSY,
        (_, Command::Rewind { .. }) => REWIND_BUSY,
        (_, Command::ReplaceInput { .. }) => REPLACE_BUSY,
        (_, Command::ChangeDirectory { .. } | Command::ChangeDirectoryRecorded { .. }) => {
            DIRECTORY_BUSY
        }
        _ => RESTORE_BUSY,
    }
}
