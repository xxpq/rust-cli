//! 会话命令的准入（docs/specs/rust-cli/session-lifecycle.md §10.4，v5 的 K-S1）：一个纯函数，输入是会话此刻的准入视图与命令，
//! 输出是放行（现在执行，或输入怎么排队）与拒绝（闭集的原因）。
//!
//! 会话 actor 用它裁决（命令过账本之后、分派之前）；v5 前端发布它对一组动作的结果；TUI 可以用它判断现在能不能发。以前每个
//! 处理函数就地判断「忙不忙」，拒绝只有给人看的文字。

use serde::{Deserialize, Serialize};

use crate::ids::{CommandId, TurnId};
use crate::session_api::{Command, Delivery};

/// 拒绝的原因（闭集）。前端按它分支、选文案；`Ack::Rejected.reason` 里的文字只作诊断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Refusal {
    /// 有回合（含压缩）或会话的后台工作（SessionStart、代码回退）在跑；这条命令只在空闲时接受。
    Busy,
    /// 没有可以停止的东西在跑。
    NotRunning,
    /// 命令指名的回合已经不是在跑的那个（§10.5）：`current` 是在跑的回合。
    StopTargetChanged { current: TurnId },
    /// 排队的输入扣住了（§10.6）：要先决定扣住的队列怎么处理（放开、或取回）。
    QueueChoiceRequired,
    /// 处置扣住的队列时，前端看到的队列与现在扣住的不同（§10.16）：`current` 是现在扣住的输入（命令 id，按队列顺序）。
    HeldQueueChanged { current: Vec<CommandId> },
    /// 排队的项与前端看到的不同（§10.21：正文或位置变了，v5 的实体版本不符）：什么都没改，前端按现在的值重新问。
    Stale,
    /// 已经有在跑、提升了还没开、或排队的压缩（§10.23，02 §4.2 的 `compactLocked`）。
    CompactLocked,
    /// 指名的排队项被编辑器锁着（§10.59）：别的编辑器加锁、取回、挪动、提升、不带锁改它。
    QueueItemLocked,
    /// 带锁改排队项时，锁已不是这个编辑器的（被放开或换主，§10.59）。
    EditLockLost,
    /// 会话正在关闭。
    Closed,
    /// 同一个命令 id 已经用于另一条命令（session-lifecycle.md §10.3）。
    CommandIdConflict,
    /// 命令指向的东西不在：消息、交互、排队的输入、同伴消息、后台任务。
    NotFound,
    /// 命令的前提不成立（没有可以恢复的输入、没有要回退的改动、起不出名字、远端没连上……）：原因在文字里。
    Unavailable,
    /// 执行时失败（存储、执行环境）：原因在文字里。
    Failed,
}

/// 会话此刻在做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Idle,
    /// 一个回合在跑（压缩也是回合）。
    Turn(TurnId),
    /// 会话的后台工作在跑：SessionStart（能被中断取消）或代码回退（不能中途放弃）。
    Job {
        cancellable: bool,
    },
}

/// 准入要看的会话状态：会话 actor 按自己的状态给出，同一时刻的值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionView {
    pub activity: Activity,
    /// 正在关闭：前台工作收尾后 actor 退出，不再接命令。
    pub closing: bool,
    /// 排队的输入扣住了、队列里还有输入（§10.6）。
    pub held: bool,
    /// 第一条 later 输入被编辑器锁着（§10.59）：它不出队，空闲时新的输入要排到它后面。
    pub frozen: bool,
    /// 有在跑、提升了还没开、或排队的压缩（§10.23）。
    pub compaction: bool,
    /// 会话的跟随方式（§10.27）：[`Command::SubmitFollowUp`] 在回合在跑时怎样投递。
    pub follow_up: Delivery,
}

/// 准入的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// 现在执行。
    Run,
    /// 输入排进队列：`Queue` 等回合结束后开回合，`Steer` 在下一个工具边界并入。
    Enqueue(Delivery),
    /// 命令要的状态已经成立，什么都不做（指名停止一个已经结束的回合，§10.5）。
    Unchanged,
    Deny(Refusal),
}

/// 一条命令此刻的准入（规则表见 session-lifecycle.md §10.4，写的是 rust 现在的行为）。
pub fn admit(view: &AdmissionView, command: &Command) -> Admission {
    if view.closing {
        return Admission::Deny(Refusal::Closed);
    }
    let busy = view.activity != Activity::Idle;
    // 空闲、队列扣住时输入要先等用户决定扣住的队列怎么处理（02 §4.1 的 choice）；运行中照常排队。
    // 空闲、没有扣住而队首被锁着（§10.59，照 v4 的 computeInputRouting）：队列只是在等编辑结束，输入排到队尾，不越过冻结的项。
    let input = if view.held {
        Admission::Deny(Refusal::QueueChoiceRequired)
    } else if view.frozen {
        Admission::Enqueue(Delivery::Queue)
    } else {
        Admission::Run
    };
    match command {
        // 指名的插话（§10.5）：只并入它指名的回合。没有回合在跑时按普通输入（02 §11.2 D：之前到与之后到结果相同）；
        // 别的回合在跑、或会话的后台工作在跑时排到回合结束后。带挂在开回合的输入上的事实的输入不并入（§10.14；执行配置不算，§10.25）。
        Command::Submit {
            delivery: Delivery::Steer,
            target: Some(target),
            extras,
            ..
        } => match &view.activity {
            Activity::Turn(current) if current == target && !extras.own_turn() => {
                Admission::Enqueue(Delivery::Steer)
            }
            Activity::Idle => input,
            Activity::Turn(_) | Activity::Job { .. } => Admission::Enqueue(Delivery::Queue),
        },
        // 按会话的跟随方式投递（§10.27）：判的那一刻在跑的就是它要并入的回合，所以不用指名。插话只并入回合，后台工作在跑、
        // 自己一轮的输入一律排队（同指名的插话）。
        Command::SubmitFollowUp { extras, .. } => match &view.activity {
            Activity::Idle => input,
            Activity::Turn(_) if view.follow_up == Delivery::Steer && !extras.own_turn() => {
                Admission::Enqueue(Delivery::Steer)
            }
            Activity::Turn(_) | Activity::Job { .. } => Admission::Enqueue(Delivery::Queue),
        },
        // 现在发（§10.27，13 Q17）：回合在跑时现在执行，由会话 actor 在同一步里抢占它；不中止会话的后台工作，排到它之后。
        Command::SubmitNow { .. } => match &view.activity {
            Activity::Idle => input,
            Activity::Turn(_) => Admission::Run,
            Activity::Job { .. } => Admission::Enqueue(Delivery::Queue),
        },
        // 「现在开」（§10.13）：扣住时也开（这正是它的用途），在跑时没有意义。
        Command::SubmitKeepingQueue { .. } if busy => Admission::Deny(Refusal::Busy),
        Command::SubmitKeepingQueue { .. } => Admission::Run,
        // 「取空扣住的队列再发」（§10.16）：这就是对扣住的处置，不再要求选择；之后同普通输入（在跑时排队，不插话）。
        Command::SubmitDiscardingQueue { .. } if busy => Admission::Enqueue(Delivery::Queue),
        Command::SubmitDiscardingQueue { .. } => Admission::Run,
        // v5 的压缩（§10.23，02 §4.2）：一次只有一个压缩在跑或在等；忙或队列扣住时排到队尾（不绕过已排的输入）。
        Command::CompactOrQueue { .. } if view.compaction => {
            Admission::Deny(Refusal::CompactLocked)
        }
        Command::CompactOrQueue { .. } if busy || view.held || view.frozen => {
            Admission::Enqueue(Delivery::Queue)
        }
        // 提升一条排队的输入（§10.13）：同指名停止，只打断它指名的回合；没指名就不打断任何回合。
        Command::Promote { target, .. } => match &view.activity {
            Activity::Idle => Admission::Run,
            Activity::Turn(current) if target.as_ref() == Some(current) => Admission::Run,
            Activity::Turn(current) => Admission::Deny(Refusal::StopTargetChanged {
                current: current.clone(),
            }),
            Activity::Job { .. } => Admission::Deny(Refusal::Busy),
        },
        // 带正文之外事实的输入自己开一轮，运行中一律排队、不插话（§10.14：材料与输入记录挂在开回合的输入上）。执行配置不算（§10.25）。
        Command::Submit { extras, .. } if busy && extras.own_turn() => {
            Admission::Enqueue(Delivery::Queue)
        }
        Command::Submit { delivery, .. } if busy => Admission::Enqueue(*delivery),
        // 带内容块的输入与 bash 模式运行中一律排到回合结束后（不插话，02 §12 K-D2）。
        Command::SubmitContent { .. } | Command::Bash { .. } if busy => {
            Admission::Enqueue(Delivery::Queue)
        }
        Command::Submit { .. } | Command::SubmitContent { .. } | Command::Bash { .. } => input,
        // 指名的停止（§10.5）：只停它指名的回合；它已经结束时什么都不做（重放与迟到都安全）。
        Command::Interrupt {
            target: Some(target),
        } => match &view.activity {
            Activity::Turn(current) if current == target => Admission::Run,
            Activity::Turn(current) => Admission::Deny(Refusal::StopTargetChanged {
                current: current.clone(),
            }),
            Activity::Idle | Activity::Job { .. } => Admission::Unchanged,
        },
        Command::Interrupt { target: None } => match view.activity {
            Activity::Turn(_) | Activity::Job { cancellable: true } => Admission::Run,
            Activity::Idle | Activity::Job { cancellable: false } => {
                Admission::Deny(Refusal::NotRunning)
            }
        },
        // 换掉一条输入（§10.53）：回合在跑时现在执行，由会话 actor 抢占它（同 `SubmitNow`）；压缩在跑或排着、会话的后台工作在跑时不换
        // （02 §4.2 的「非 compacting」；代码回退的事务不能中途放弃）。扣住的队列不挡它：它不经队列，队列照旧扣住。
        Command::ReplaceInput { .. } => match view.activity {
            Activity::Idle => Admission::Run,
            Activity::Turn(_) if !view.compaction => Admission::Run,
            Activity::Turn(_) | Activity::Job { .. } => Admission::Deny(Refusal::Busy),
        },
        // v5 的 fork（§10.57）：不打断在跑的回合，目标这一轮是否已经结束由会话 actor 按回合记录判；压缩在跑或排着、会话的后台工作在跑时不分
        // （02 §4.2 的「非 compacting」；同 `ReplaceInput`）。TUI 的 `ForkFromInput` 照旧只在空闲时（分叉之后它关掉父会话、切过去）。
        Command::ForkFromInputAs { .. } => match view.activity {
            Activity::Idle => Admission::Run,
            Activity::Turn(_) if !view.compaction => Admission::Run,
            Activity::Turn(_) | Activity::Job { .. } => Admission::Deny(Refusal::Busy),
        },
        // v5 的侧聊（§10.58）：只读已提交的记录、不改这个会话，什么时候都执行（关闭中在最前面拒绝）。
        Command::SideChatAs { .. } => Admission::Run,
        Command::Compact { .. }
        | Command::Fork { .. }
        | Command::ForkFromInput { .. }
        | Command::Rewind { .. }
        | Command::RestoreCancelled
        | Command::ChangeDirectory { .. }
        | Command::ChangeDirectoryRecorded { .. }
            if busy =>
        {
            Admission::Deny(Refusal::Busy)
        }
        _ => Admission::Run,
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
