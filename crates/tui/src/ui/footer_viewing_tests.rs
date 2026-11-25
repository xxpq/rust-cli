//! 查看子代理时状态栏与槽跟着切（docs/specs/rust-cli/tui-panels.md §1.7，修复线 F112，用户要求 2026-10-29；样子照 Puffin，
//! tui-kit.md §4.2「状态栏」）：模型、档位、速度、上下文、cache、上下文提示换成被查看的子代理的；权限模式与 plan
//! 一直是主会话的，⇧⇥ 切换主会话的模式（2026-11-12）；查看时槽里不写 esc 中断与不生效的按键提示；返回主会话后恢复。

use rust_cli_kernel_api::compaction::{ContextNotice, ContextOccupancy};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::metering::{OutputSpeed, RequestStats};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, Running, SequencedEvent, SessionView};
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};
use rust_cli_kernel_api::testing::selection;

use rust_cli_tui_kit::render::StyledLine;

use super::*;
use crate::ui::agents::{AgentList, Viewing};
use crate::ui::app::{Action, Overlay};
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn event(app: &mut App, seq: u64, event: KernelEvent) {
    app.view.apply(
        SequencedEvent {
            seq,
            revision: 0,
            event,
        },
        0,
    );
}

/// 主会话：模型 `scripted`、TPS 40、ctx 37.0%、命中率 91.0%（只在 `/stats` 的卡里）、`10% until auto-compact`，回合在跑，输入框为空。
fn main_app(lang: Lang) -> App {
    let mut app = App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))).with_foreground(Some((230, 230, 230))),
        lang,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.session = Some("sess_main".into());
    event(
        &mut app,
        1,
        KernelEvent::ContextUsage {
            used_tokens: 66_600,
            max_tokens: 180_000,
            compact_at: None,
            window: None,
        },
    );
    event(
        &mut app,
        2,
        KernelEvent::OutputSpeed {
            speed: OutputSpeed {
                output_tokens: Some(120),
                duration_ms: 5_000,
                first_content_ms: Some(2_000),
            },
        },
    );
    event(
        &mut app,
        3,
        KernelEvent::ContextNotice {
            notice: ContextNotice::UntilAutoCompact { percent: 10 },
        },
    );
    event(
        &mut app,
        4,
        KernelEvent::TurnStarted {
            turn: rust_cli_kernel_api::ids::TurnId::new("turn_1"),
            command: None,
            origin: rust_cli_kernel_api::session_api::TurnOrigin::Input,
            started_at: 0,
            works: Vec::new(),
        },
    );
    receipt(&mut app, 2_000, 1_820);
    // 画一次：主会话的速度是画之前听说视图的现场估算（status_motion.rs）。
    let started = app.started;
    crate::ui::status_motion::note(&mut app, started, 0);
    app
}

/// 请求统计的命中率（2026-11-12 以前在 `/stats` 卡上，之后状态栏读它）。
fn hit_rate(app: &App) -> Option<f64> {
    app.snapshots.request_stats.stats().cache_hit_rate()
}

/// 一份命中率的回执（累计输入、缓存读）。
fn receipt(app: &mut App, input: u64, read: u64) {
    let seq = app.snapshots.request_stats.begin();
    assert!(app.snapshots.request_stats.arrived(
        seq,
        Some(RequestStats {
            requests: 1,
            input_tokens: input,
            cache_read_tokens: read,
            unknown_cache_usage: false,
            last: None,
            ..RequestStats::default()
        })
    ));
}

/// 子代理的快照：自己的模型与档位、权限模式；`stats` 时带 ctx 20.0%（9000 / 45000）与 30 tok/s。
fn subagent_view(mode: PermissionMode, plan: bool, stats: bool) -> Transcript {
    let view = SessionView {
        plan,
        running: Some(Running::Subagent),
        context_usage: stats.then_some(ContextOccupancy {
            used_tokens: 9_000,
            max_tokens: 45_000,
            compact_at: None,
            window: None,
        }),
        context_notice: ContextNotice::Hidden,
        output_speed: stats.then_some(OutputSpeed {
            output_tokens: Some(90),
            duration_ms: 4_000,
            first_content_ms: Some(1_000),
        }),
        ..SessionView::new(
            SessionMeta {
                id: SessionId::new("sess_subagent_agent_a"),
                kind: SessionKind::Subagent,
                parent: Some(SessionId::new("sess_main")),
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new("/Users/dev/project"),
                },
                title: "Find callers".into(),
                trace: TraceId::new("sess_main"),
                created_at_ms: 0,
            },
            Vec::new(),
            ModelSelection {
                model: "child-model".into(),
                reasoning: Some("high".into()),
                ..selection()
            },
            mode,
        )
    };
    Transcript::from_view(view, 0)
}

fn entry() -> SubagentEntry {
    SubagentEntry {
        agent: AgentId::new("agent_a"),
        child: SessionId::new("sess_subagent_agent_a"),
        agent_type: "Explore".into(),
        description: "Find callers".into(),
        status: SubagentStatus::Running,
        background: true,
        waiting: false,
        elapsed_ms: 5_000,
        ended_ago_ms: None,
        version: 0,
        owner: None,
        activity: None,
        parked: false,
        machine: Default::default(),
        tool_use: None,
        started_at_ms: 0,
        ended_at_ms: None,
    }
}

/// 开始查看 agent_a：runner 在查看对象换了时清空命中率的回执再查子会话（这里直接给回执）。
fn view(app: &mut App, view: Transcript) {
    app.snapshots.agents = AgentList {
        entries: vec![entry()],
        at: Some(0),
        shown: 0,
    };
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: "Find callers".into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view,
    });
    app.snapshots.request_stats.clear();
}

/// 状态栏（终端 30 行）与输入行（带槽）的文字，换行分开。
fn plain(app: &App, width: usize) -> String {
    let bar = status(app, width, Vec::new(), false);
    let mut text: Vec<String> = bar.iter().map(StyledLine::plain).collect();
    text.push(crate::ui::pane::input_line(app, width).plain());
    text.join("\n")
}

/// 只看状态栏、终端 10 行（总是一行）。
fn bar(app: &App, width: usize) -> String {
    status(app, width, Vec::new(), false)[0].plain()
}

/// 状态栏换成子代理的模型、档位、速度、上下文与 cache；主会话的值（含上下文提示）一个都不留。修前只读主会话。
#[test]
fn viewing_shows_the_subagents_model_effort_and_stats() {
    let mut app = main_app(Lang::En);
    let before = bar(&app, 160);
    for main in [
        "scripted",
        "ctx 37.0%",
        "40 tok/s",
        "10% until auto-compact",
    ] {
        assert!(before.contains(main), "{main}: {before}");
    }
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, true),
    );
    receipt(&mut app, 400, 200);
    let text = bar(&app, 160);
    // 缓存命中也是被查看的子代理的（状态栏的缓存，tui-display.md §2.6）。
    for sub in ["child-model · high", "ctx 20.0%", "30 tok/s", "cache 50.0%"] {
        assert!(text.contains(sub), "{sub}: {text}");
    }
    assert_eq!(hit_rate(&app), Some(0.5));
    for main in ["scripted", "40 tok/s", "37.0%", "auto-compact"] {
        assert!(!text.contains(main), "{main}: {text}");
    }
    // 模型 fg2、档位 fg3、数 fg2（Puffin 的状态栏）。
    let lines = status(&app, 160, Vec::new(), false);
    let style = |text: &str| {
        lines[0]
            .spans
            .iter()
            .find(|s| s.text == text)
            .unwrap_or_else(|| panic!("{text}"))
            .style
    };
    assert_eq!(style("child-model"), app.ink.theme.fg2);
    assert_eq!(style("high"), app.ink.theme.fg3);
    assert_eq!(style("20.0%"), app.ink.theme.fg2);
}

/// 子代理没有的项不出现，也不拿主会话的值顶替：还没有完成的请求（没有速度、没有上下文）、命中率的回执还没到。
#[test]
fn missing_subagent_stats_are_left_out() {
    let mut app = main_app(Lang::En);
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, false),
    );
    let text = bar(&app, 160);
    assert!(text.contains("child-model · high"), "{text}");
    for main in ["tok/s", "%", "auto-compact"] {
        assert!(!text.contains(main), "{main}: {text}");
    }
}

/// 子代理的上下文提示与上下文同一处算，跟着切（暂定，tui-panels.md D20）。
#[test]
fn the_context_notice_is_the_subagents() {
    let mut app = main_app(Lang::En);
    let mut sub = SessionView {
        context_notice: ContextNotice::Low {
            percent: 8,
            compact_hint: false,
        },
        ..snapshot_of(subagent_view(PermissionMode::Default, false, true))
    };
    sub.running = None;
    view(&mut app, Transcript::from_view(sub, 0));
    let text = bar(&app, 200);
    assert!(text.contains("Context low (8% remaining)"), "{text}");
    assert!(!text.contains("until auto-compact"), "{text}");
}

/// 左边：权限模式与 plan 写主会话此刻的，`⇧⇥` 照常写（2026-11-12，tui-panels.md §1.7；修改前写子代理派生时的
/// `Plan · Edit automatically`、没有 `⇧⇥`）。槽里不写 `esc stop`（esc 是返回），也不写查看时不生效的按键提示。
#[test]
fn the_left_side_shows_the_main_mode_without_main_hints() {
    let mut app = main_app(Lang::En);
    // 主会话有一个在跑的 shell（pill）。
    app.snapshots.background = vec![rust_cli_kernel_api::manage::BackgroundEntry::new(
        "bash_1",
        rust_cli_kernel_api::manage::BackgroundKind::Shell,
        "npm run dev",
        rust_cli_kernel_api::env_shell::BackgroundStatus::Running,
    )];
    let before = plain(&app, 160);
    assert!(before.contains("Ask before changes ⇧⇥"), "{before}");
    assert!(before.contains("esc stop"), "{before}");
    view(
        &mut app,
        subagent_view(PermissionMode::AcceptEdits, true, true),
    );
    let text = plain(&app, 160);
    assert!(text.starts_with("   Ask before changes ⇧⇥   "), "{text}");
    assert!(!text.contains("Edit automatically"), "{text}");
    assert!(text.contains("· child-model · high"), "{text}");
    // 后台命令的 pill 并进了任务行（tui-tasks.md §4）：状态栏里没有。
    assert!(!text.contains("1 shell"), "{text}");
    for hint in ["esc stop", "to manage", "for projects", "for shortcuts"] {
        assert!(!text.contains(hint), "{hint}: {text}");
    }
    // 主会话空闲、有任务清单时的 `ctrl+t` 提示也不写。
    event(
        &mut app,
        5,
        KernelEvent::TurnFinished {
            turn: rust_cli_kernel_api::ids::TurnId::new("turn_1"),
            outcome: rust_cli_kernel_api::session_api::TurnOutcome::Success,
            usage: None,
            time: rust_cli_kernel_api::session_api::TurnTime {
                ended_at: 0,
                active_ms: 0,
            },
            files: Vec::new(),
        },
    );
    event(
        &mut app,
        6,
        KernelEvent::TasksChanged {
            tasks: serde_json::json!([{"id": "1", "subject": "task 1", "description": "",
                                       "status": "pending", "blocks": [], "blockedBy": []}]),
        },
    );
    app.snapshots.background.clear();
    let text = plain(&app, 160);
    assert!(!text.contains("ctrl+t"), "{text}");
    assert!(!text.contains("for projects"), "{text}");
    // 上滚时槽里不写 `esc latest`：回到末尾的键在分隔行（`↓ N more   esc back to the end`，tui-kit.md §4.0）。
    app.port.scroll = 3;
    let text = plain(&app, 160);
    assert!(!text.contains("esc latest"), "{text}");
}

/// 查看时按 ⇧⇥ 切换主会话的模式（tui-panels.md §1.7）：从主会话的模式往下一个；切到完全访问同样先弹确认框。
/// 修改前查看时 ⇧⇥ 什么都不做。
#[test]
fn shift_tab_while_viewing_switches_the_main_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = main_app(Lang::En);
    view(
        &mut app,
        subagent_view(PermissionMode::AcceptEdits, false, true),
    );
    let tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    let now = std::time::Instant::now();
    assert!(matches!(
        app.on_key(tab, now),
        Some(Action::SetMode(PermissionMode::Guarded))
    ));
    // 主会话到了自主：下一个是完全访问，先弹确认框，不切。
    let selection = app.view.selection().clone();
    let guarded = app.view.execution().with_mode(PermissionMode::Guarded);
    app.view.set_draft(selection, guarded);
    assert_eq!(app.on_key(tab, now), None);
    assert!(
        matches!(app.overlay, Some(Overlay::YoloConfirm(_))),
        "{:?}",
        app.overlay.is_some()
    );
    assert!(app.viewing.is_some());
}

/// 返回主会话：模型、速度、上下文、提示立即是主会话的（它们一直在主会话的视图里），`/stats` 卡的命中率等新回执；左边恢复。
#[test]
fn returning_to_main_restores_it() {
    let mut app = main_app(Lang::En);
    view(
        &mut app,
        subagent_view(PermissionMode::AcceptEdits, false, true),
    );
    receipt(&mut app, 400, 200);
    app.viewing = None;
    app.snapshots.request_stats.clear();
    let text = plain(&app, 200);
    for main in [
        "scripted",
        "ctx 37.0%",
        "40 tok/s",
        "10% until auto-compact",
    ] {
        assert!(text.contains(main), "{main}: {text}");
    }
    assert_eq!(hit_rate(&app), None);
    assert!(text.contains("Ask before changes ⇧⇥"), "{text}");
    assert!(text.contains("esc stop"), "{text}");
    receipt(&mut app, 2_000, 1_820);
    assert_eq!(hit_rate(&app), Some(0.91));
}

/// 换查看对象之前发出的命中率查询，回执晚到时丢掉（序号），不显示在新对象下面。
#[test]
fn a_receipt_from_before_the_switch_is_dropped() {
    let mut app = main_app(Lang::En);
    let in_flight = app.snapshots.request_stats.begin();
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, true),
    );
    assert!(!app.snapshots.request_stats.arrived(
        in_flight,
        Some(RequestStats {
            requests: 1,
            input_tokens: 2_000,
            cache_read_tokens: 1_820,
            unknown_cache_usage: false,
            last: None,
            ..RequestStats::default()
        })
    ));
    assert_eq!(hit_rate(&app), None);
}

/// 查谁的命中率：没有会话时不查；看主会话时是主会话；查看时是列表快照里那个条目的子会话；条目不在了不查。
#[test]
fn the_stats_follow_the_viewed_session() {
    let mut app = main_app(Lang::En);
    assert_eq!(app.stats_session(), Some(SessionId::new("sess_main")));
    view(
        &mut app,
        subagent_view(PermissionMode::Default, false, true),
    );
    assert_eq!(
        app.stats_session(),
        Some(SessionId::new("sess_subagent_agent_a"))
    );
    app.snapshots.agents.entries.clear();
    assert_eq!(app.stats_session(), None);
    app.viewing = None;
    app.session = None;
    assert_eq!(app.stats_session(), None);
}

/// 中文：模式名按界面语言（主会话的模式，tui-panels.md §1.7）；槽里不写 esc 中断。
#[test]
fn chinese_uses_the_chinese_words() {
    let mut app = main_app(Lang::Zh);
    view(
        &mut app,
        subagent_view(PermissionMode::AcceptEdits, false, true),
    );
    receipt(&mut app, 400, 200);
    let text = plain(&app, 200);
    assert!(text.contains("30 tok/s"), "{text}");
    assert!(text.contains("变更前确认 ⇧⇥"), "{text}");
    assert!(!text.contains("自动编辑"), "{text}");
    assert!(!text.contains("esc 中断"), "{text}");
}

/// 窄终端：查看时也按名次丢、不换行（tui-display.md §2.6）——速度、缓存、档位先走，上下文提示（80）在上下文（85）之前走，
/// 模型（90）最后走；一个回不来时名次更低的也不回来。
#[test]
fn narrow_terminals_keep_the_order_while_viewing() {
    let mut app = main_app(Lang::En);
    let mut sub = snapshot_of(subagent_view(PermissionMode::Default, false, true));
    sub.context_notice = ContextNotice::UntilAutoCompact { percent: 7 };
    view(&mut app, Transcript::from_view(sub, 0));
    receipt(&mut app, 400, 200);
    let mut seen: Vec<String> = Vec::new();
    for width in (20..=200).rev() {
        let text = bar(&app, width);
        let right: Vec<&str> = [
            "30 tok/s",
            "cache 50.0%",
            "ctx 20.0%",
            "7% until auto-compact",
            "child-model",
            "high",
        ]
        .into_iter()
        .filter(|item| text.contains(item))
        .collect();
        let right = right.join(" | ");
        if seen.last() != Some(&right) {
            seen.push(right);
        }
    }
    assert_eq!(
        seen,
        [
            "30 tok/s | cache 50.0% | ctx 20.0% | 7% until auto-compact | child-model | high",
            "cache 50.0% | ctx 20.0% | 7% until auto-compact | child-model | high",
            "ctx 20.0% | 7% until auto-compact | child-model | high",
            "ctx 20.0% | 7% until auto-compact | child-model",
            "ctx 20.0% | child-model",
            "child-model",
            "",
        ]
    );
}

/// 测试里改快照的一项：把子代理视图的设置还原成快照（只取 footer 用到的字段）。
fn snapshot_of(view: Transcript) -> SessionView {
    SessionView {
        plan: view.plan(),
        running: view.running().then_some(Running::Subagent),
        context_usage: view.context_usage(),
        context_notice: view.context_notice(),
        output_speed: view.output_speed(),
        ..SessionView::new(
            SessionMeta {
                id: SessionId::new("sess_subagent_agent_a"),
                kind: SessionKind::Subagent,
                parent: Some(SessionId::new("sess_main")),
                workspace: view.workspace().clone(),
                title: view.title().to_owned(),
                trace: TraceId::new("sess_main"),
                created_at_ms: 0,
            },
            Vec::new(),
            view.selection().clone(),
            view.mode(),
        )
    }
}
