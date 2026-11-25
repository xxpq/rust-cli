//! 后台工作表的规则（session-lifecycle.md §10.28）：开始与结束各发一次；这一轮里终态先到的开始不发；回合结束清掉先到的终态。
//! 结束了的记下终态（§10.44），供输出详情在结束之后读。
use super::*;

const DONE: BackgroundStatus = BackgroundStatus::Completed;

fn start(id: &str, kind: BackgroundKind) -> BackgroundStart {
    BackgroundStart {
        id: id.to_owned(),
        kind,
        description: format!("run {id}"),
        tool_use: Some(ToolUseId::new(format!("toolu_{id}"))),
        child: None,
        started_at: 7,
    }
}

#[test]
fn a_work_starts_and_ends_once() {
    let mut works = Works::default();
    let started = works.started(start("b1", BackgroundKind::Shell));
    assert!(matches!(started, Some(KernelEvent::BackgroundStarted { work }) if work.id == "b1"));
    assert_eq!(works.running().len(), 1);
    assert_eq!(works.kind_of("b1"), Some(BackgroundKind::Shell));
    // 同一项再开始一次（续跑停放的子代理）不重复
    assert_eq!(works.started(start("b1", BackgroundKind::Shell)), None);
    assert!(
        matches!(works.ended("b1", DONE, true), Some(KernelEvent::BackgroundEnded { work }) if work == "b1")
    );
    assert_eq!(works.ended("b1", DONE, true), None);
    assert!(works.running().is_empty());
}

#[test]
fn an_end_that_beats_its_start_in_the_turn_drops_the_start() {
    let mut works = Works::default();
    assert_eq!(works.ended("b1", DONE, true), None);
    assert_eq!(works.started(start("b1", BackgroundKind::Shell)), None);
    assert!(works.running().is_empty());
    // 用掉了：同一个 id 之后再开始（不会发生，只验证记录只用一次）照常进表
    assert!(works.started(start("b1", BackgroundKind::Shell)).is_some());
}

#[test]
fn an_unknown_end_outside_a_turn_is_not_remembered() {
    let mut works = Works::default();
    assert_eq!(works.ended("b1", DONE, false), None);
    assert!(works.started(start("b1", BackgroundKind::Shell)).is_some());
}

#[test]
fn the_end_of_a_turn_forgets_the_early_ends() {
    let mut works = Works::default();
    assert_eq!(works.ended("agent_1", DONE, true), None);
    works.turn_ended();
    assert!(
        works
            .started(start("agent_1", BackgroundKind::Agent))
            .is_some()
    );
}

#[test]
fn works_are_listed_in_start_order() {
    let mut works = Works::default();
    works.started(start("b2", BackgroundKind::Shell));
    works.started(start("agent_1", BackgroundKind::Agent));
    works.started(start("b0", BackgroundKind::Monitor));
    let ids: Vec<String> = works.running().into_iter().map(|work| work.id).collect();
    assert_eq!(ids, ["b2", "agent_1", "b0"]);
}

#[test]
fn a_finished_work_keeps_its_terminal_status() {
    let mut works = Works::default();
    works.started(start("b1", BackgroundKind::Shell));
    works.started(start("b2", BackgroundKind::Monitor));
    assert_eq!(
        works.state("b1"),
        Some((BackgroundKind::Shell, BackgroundStatus::Running))
    );
    works.ended("b1", BackgroundStatus::Failed, false);
    works.ended("b2", BackgroundStatus::Killed, false);
    assert_eq!(
        works.state("b1"),
        Some((BackgroundKind::Shell, BackgroundStatus::Failed))
    );
    assert_eq!(
        works.state("b2"),
        Some((BackgroundKind::Monitor, BackgroundStatus::Killed))
    );
    // 不认识的结束不记：表里没有它
    works.ended("b9", DONE, false);
    assert_eq!(works.state("b9"), None);
}

#[test]
fn only_the_latest_finished_works_are_kept() {
    let mut works = Works::default();
    for index in 0..=FINISHED_RETAIN {
        let id = format!("b{index}");
        works.started(start(&id, BackgroundKind::Shell));
        works.ended(&id, DONE, false);
    }
    assert_eq!(works.state("b0"), None);
    assert_eq!(works.state("b1"), Some((BackgroundKind::Shell, DONE)));
    assert_eq!(
        works.state(&format!("b{FINISHED_RETAIN}")),
        Some((BackgroundKind::Shell, DONE))
    );
}

/// 开回合的后台工作（§10.50）：命令与 monitor 的终态通知自带任务 ID、种类与说明；子代理的通知只带 ID，说明按后台工作表找——结束了的项也记着
/// 说明（通知在回合结束之后才开回合，那时它已经不在跑）；表里没有时说明为空。同伴的消息与 monitor 的事件行不是一项后台工作的终态，没有。
#[test]
fn a_notice_names_its_work_and_an_ended_agent_keeps_its_description() {
    use rust_cli_kernel_api::agents::AgentNotice;
    use rust_cli_kernel_api::background::NoticedWork;
    use rust_cli_kernel_api::ids::AgentId;
    use rust_cli_kernel_api::machine_notices::MachineNotice;

    let mut works = Works::default();
    works.started(start("agent_1", BackgroundKind::Agent));
    works.ended("agent_1", DONE, false);
    let agent = |id: &str| {
        crate::lane::Waiting::Agent(AgentNotice {
            seq: 1,
            agent: AgentId::new(id),
            status: DONE,
            text: "<task-notification>".to_owned(),
        })
    };
    assert_eq!(
        agent("agent_1").work(&works),
        Some(NoticedWork {
            id: "agent_1".to_owned(),
            kind: BackgroundKind::Agent,
            description: "run agent_1".to_owned(),
        })
    );
    assert_eq!(
        agent("agent_x").work(&works).map(|work| work.description),
        Some(String::new())
    );
    let shell = crate::lane::Waiting::Shell(MachineNotice {
        machine: rust_cli_kernel_api::ids::MachineName::primary(),
        notice: rust_cli_kernel_api::testing::shell_notice(1, "b1"),
    });
    assert_eq!(
        shell.work(&works),
        Some(NoticedWork {
            id: "b1".to_owned(),
            kind: BackgroundKind::Shell,
            description: "Run b1".to_owned(),
        })
    );
    assert_eq!(
        crate::lane::Waiting::MonitorEvent("line".to_owned()).work(&works),
        None
    );
}
