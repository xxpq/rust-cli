//! SendMessage 的去向（[`Live::deliver`]，agents.md §6）：在一把锁里按条目的状态定排队、续跑或拒绝。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::testing::ManualClock;

use super::*;
use crate::report::{Ending, Report, StoppedBy};
use crate::test_support::running;

const ROOT: &str = "sess_root";

fn live_with(state: Option<AgentState>) -> (Live, AgentId) {
    let live = Live::new(Arc::new(ManualClock::default()));
    let mut entry = running("agent_a", ROOT);
    if let Some(state) = state {
        entry.state = state;
    }
    live.insert(entry);
    (live, AgentId::new("agent_a"))
}

fn deliver(live: &Live, agent: &AgentId) -> Option<Delivery> {
    live.deliver(
        agent,
        &SessionId::new(ROOT),
        "Go on.".to_owned(),
        ToolUseId::new("toolu_send"),
    )
}

/// 停了、还在收尾的运行一律不排队（排进去也会在运行结束时丢掉）：用户停的回 `StoppedByUser`，TaskStop 停的与前台随父调用被取消的回
/// `StillStopping`（照对照实现的 still stopping，EXP-DEFECTS-5），邮箱不动。结束了的只有用户停的不续跑（照对照实现的 `stoppedByUser`）。
/// 以前 TaskStop 停了、还在收尾的照常排进邮箱（EXP-DEFECTS-4 之前用户停的也是），结束了的照常续跑。
#[test]
fn a_send_message_to_a_stopped_agent_is_never_queued() {
    let (live, agent) = live_with(None);
    assert!(matches!(deliver(&live, &agent), Some(Delivery::Queued)));
    for by in [StoppedBy::User, StoppedBy::Model, StoppedBy::CallCancelled] {
        let (live, agent) = live_with(None);
        assert!(live.stop(&agent, by));
        let delivered = deliver(&live, &agent);
        if by == StoppedBy::User {
            assert!(matches!(delivered, Some(Delivery::StoppedByUser)), "{by:?}");
        } else {
            assert!(matches!(delivered, Some(Delivery::StillStopping)), "{by:?}");
        }
        let AgentState::Running { mailbox, .. } = live.get(&agent).unwrap().state else {
            panic!("{by:?}: still running until it exits");
        };
        assert!(mailbox.is_empty(), "{by:?}");
    }
    for ending in [
        Ending::Killed(StoppedBy::User),
        Ending::KilledParked(StoppedBy::User),
    ] {
        let (live, agent) = live_with(Some(AgentState::Finished(ending)));
        assert!(
            matches!(deliver(&live, &agent), Some(Delivery::StoppedByUser)),
            "{ending:?}"
        );
        assert!(matches!(
            live.get(&agent).unwrap().state,
            AgentState::Finished(_)
        ));
    }
    for state in [
        AgentState::Finished(Ending::Killed(StoppedBy::Model)),
        AgentState::Finished(Ending::KilledParked(StoppedBy::Model)),
        AgentState::Finished(Ending::Killed(StoppedBy::CallCancelled)),
        AgentState::Finished(Ending::Completed),
        AgentState::Finished(Ending::Failed),
        AgentState::Parked {
            report: Report::default(),
            notified: false,
        },
    ] {
        let label = format!("{state:?}");
        let (live, agent) = live_with(Some(state));
        let Some(Delivery::Restarted { agent: running, .. }) = deliver(&live, &agent) else {
            panic!("{label} resumes");
        };
        assert_eq!(running.tool_use, Some(ToolUseId::new("toolu_send")));
        assert!(matches!(running.state, AgentState::Running { .. }));
    }
}

/// 不是这个会话启动的子代理：交给同伴会话的寻址（None），条目不动。
#[test]
fn an_agent_of_another_session_is_not_delivered_to() {
    let (live, agent) = live_with(Some(AgentState::Finished(Ending::Completed)));
    assert!(
        live.deliver(
            &agent,
            &SessionId::new("sess_other"),
            String::new(),
            ToolUseId::new("toolu_send"),
        )
        .is_none()
    );
    assert!(matches!(
        live.get(&agent).unwrap().state,
        AgentState::Finished(Ending::Completed)
    ));
}
