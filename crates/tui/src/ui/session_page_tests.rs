//! 设置的 Status 段（tui-panels.md §4.1、§4.5；以前是统计的页的 Session 标签）：`/status` 打开它，esc 关上设置；`/status`、
//! `/stats`、`/context` 不是别名；事实到了才有 MCP、账号、分支与 hooks；⏎ 复制会话 ID；查看子代理时写子代理。行照以前的页从
//! 第 3 列起（设置的窗格排好之后去掉这 3 列，`settings/live.rs`）。
use std::time::Instant;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::manage::{HookEntry, McpServerInfo, McpServerState};
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::StyledLine;

use super::*;
use crate::machines::{ExecutorInfo, MachineRow, MachineStatus};
use crate::services::{Family, FamilyStatus, MemoryFile, PlanConnection};
use crate::ui::agents::{AgentList, Viewing};
use crate::ui::app::Action;
use crate::ui::context_tree::tests::sample;
use crate::ui::settings::page::Page;
use crate::ui::test_support::{app, key};
use crate::view::Transcript;

const ID: &str = "7b2c91e4-1f0a-4c55-9a8e-3d1f2a6b8c21";

fn started() -> App {
    let mut app = app();
    app.session = Some(ID.into());
    app
}

/// 会话的标题经内核的事件折进视图（同真实的会话）。
fn titled(app: &mut App) {
    app.view.apply(
        SequencedEvent {
            seq: 1,
            event: KernelEvent::TitleChanged {
                title: "Fix the footer spacing".into(),
            },
            revision: 0,
        },
        0,
    );
}

fn server(name: &str, state: McpServerState, tools: usize, reason: Option<&str>) -> McpServerInfo {
    McpServerInfo {
        name: name.into(),
        state,
        reason: reason.map(str::to_owned),
        tools,
        machine: Default::default(),
        auth: None,
    }
}

fn facts() -> SessionFacts {
    SessionFacts {
        mcp: vec![
            server("github", McpServerState::Connected, 24, None),
            server(
                "jira",
                McpServerState::Failed,
                0,
                Some("401 Unauthorized: the token has expired"),
            ),
            server("slack", McpServerState::Disabled, 0, None),
        ],
        hooks: vec![HookEntry {
            event: "PreToolUse".into(),
            matcher: None,
            kind: "command".into(),
            display: "./scripts/check.sh".into(),
            source: "user".into(),
        }],
        providers: Vec::new(),
        families: vec![FamilyStatus {
            family: Family::Zai,
            name: "Z.ai".into(),
            signed_in: true,
            account: "dev@example.com".into(),
            connection: PlanConnection::Individual,
            teams: Vec::new(),
            teams_error: String::new(),
            provider: "fixture".into(),
            models: Vec::new(),
        }],
        branch: Some("main".into()),
    }
}

fn text(app: &App, width: usize) -> Vec<String> {
    app.session_lines(width, 0)
        .iter()
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .collect()
}

/// 设置的状态栏（最后一行）：右边是这一页的键，刚复制了时是那一句。
/// 状态栏第 1 行（这一页的键）：倒数第 2 行，最后一行是目录与标题（tui-display.md §2.6）。
fn keys(app: &mut App) -> String {
    app.settings_frame(Instant::now(), 0, 100, 40)
        .expect("the settings")
        .iter()
        .rev()
        .nth(1)
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .unwrap_or_default()
}

/// `/status` 打开设置的 Status 段（页有焦点）、要问它的计数与事实；esc 关上设置。`/stats`、`/context` 是别的段，不问事实。
#[test]
fn status_opens_its_settings_section() {
    let mut app = started();
    assert_eq!(app.slash("/status"), Some(Action::CountContext));
    let ui = app.settings.as_ref().expect("the settings");
    assert_eq!(ui.page(), Page::Top("status"));
    assert!(!ui.in_list);
    assert!(app.session_facts_wanted());
    app.settings_key(key(KeyCode::Esc));
    assert!(app.settings.is_none());
    assert!(!app.session_facts_wanted());
    let _ = app.slash("/stats");
    assert_eq!(app.settings.as_ref().unwrap().page(), Page::Top("stats"));
    assert!(!app.session_facts_wanted());
    let _ = app.slash("/context");
    assert_eq!(app.settings.as_ref().unwrap().page(), Page::Top("context"));
    assert!(!app.session_facts_wanted(), "only Status asks");
}

/// 状态栏右边是 Status 的键：`⏎ copy id`，没有以前的 `⇥ context`。
#[test]
fn the_status_keys_have_copy_id_and_no_tab() {
    let mut app = started();
    let _ = app.slash("/status");
    let keys = keys(&mut app);
    assert!(keys.contains("⏎ copy id"), "{keys}");
    assert!(keys.contains("esc close"), "{keys}");
    // 左边是模式的 `⇧⇥`；右边这一页的键里没有 ⇥。
    let right = &keys[keys.find("↑↓ scroll").expect("the page keys")..];
    assert!(!right.contains('⇥'), "{keys}");
}

/// 行按 §4.5：标题、完整的 ID、模型与窗口、账号、模式、目录与分支、版本与新版本、MCP、规则文件、hooks。
#[test]
fn the_status_section_lists_the_facts() {
    let mut app = started();
    titled(&mut app);
    app.newer_release = Some("0.1.85".into());
    app.memory_files = vec![MemoryFile {
        label: "User memory".into(),
        display: "~/.zcode/AGENTS.md".into(),
        path: "/Users/dev/.zcode/AGENTS.md".into(),
    }];
    let _ = app.slash("/status");
    let before = text(&app, 100);
    assert!(
        before.iter().any(|l| l == "   Checking the servers…"),
        "{before:#?}"
    );
    assert!(!before.iter().any(|l| l.starts_with("   Account")));
    app.context_counted(Some(SessionId::new(ID)), Ok(sample()), Instant::now(), 0);
    app.session_facts_arrived(facts());
    let lines = text(&app, 100);
    let has = |want: &str| {
        assert!(lines.iter().any(|l| l == want), "{want:?} in {lines:#?}");
    };
    // 没有面包屑：设置页有自己的（以前是 `Stats  ›  Session` 与两个标签）。
    assert!(!lines.iter().any(|l| l.contains("Stats  ›")), "{lines:#?}");
    has("   Session    Fix the footer spacing");
    has(&format!("              {ID}"));
    has("   Model      scripted   Z.ai");
    has("              200k window, the model's own  ·  compacts at 160k");
    has("   Account    dev@example.com  ·  Individual Plan");
    has("   Mode       Ask before changes");
    has("   Version    zcode 0.1.0  ·  0.1.85 is out, run zcode update");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("   Directory  ") && l.ends_with("  ·  main")),
        "{lines:#?}"
    );
    let mcp = lines
        .iter()
        .find(|l| l.starts_with("   MCP servers"))
        .expect("the MCP heading");
    assert!(mcp.ends_with("1 connected  ·  1 failed"), "{mcp}");
    has("   ✓ github        24 tools");
    assert!(
        lines.iter().any(|l| l
            .starts_with("   ✗ jira          401 Unauthorized: the token has expired")
            && l.ends_with("/mcp")),
        "{lines:#?}"
    );
    has("   – slack         turned off");
    has("     ~/.zcode/AGENTS.md   User memory");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("     PreToolUse  ./scripts/check.sh") && l.ends_with("user")),
        "{lines:#?}"
    );
}

/// 没有会话时说还没开始；窄时 ID 只写头尾、没有新版本的提示与行尾的指向；没有 MCP 服务器时说没有。
#[test]
fn before_the_first_message_and_at_narrow_widths() {
    let mut app = app();
    app.newer_release = Some("0.1.85".into());
    app.session_facts_arrived(SessionFacts::default());
    let lines = text(&app, 100);
    assert!(
        lines
            .iter()
            .any(|l| l == "   Session    not started  ·  your first message starts one"),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|l| l == "   No MCP servers"), "{lines:#?}");
    let mut app = started();
    app.newer_release = Some("0.1.85".into());
    app.session_facts_arrived(facts());
    let lines = text(&app, 64);
    assert!(
        lines.iter().any(|l| l == "              7b2c91e4…6b8c21"),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|l| l == "   Version    zcode 0.1.0"));
    assert!(!lines.iter().any(|l| l.ends_with("/mcp")), "{lines:#?}");
}

/// 机器：就绪的写往返与系统，没连的写 not connected，标题右端是各自的数。
#[test]
fn machines_are_listed_when_there_are_any() {
    let mut app = started();
    app.machines_enabled = true;
    app.machines = vec![
        MachineRow {
            name: "devbox".into(),
            ssh: "dev@devbox".into(),
            working_directory: None,
            status: MachineStatus::Ready,
            latency_ms: Some(42),
            executor: Some(ExecutorInfo {
                os: "linux".into(),
                arch: "arm64".into(),
                version: "0.1.0".into(),
                refreshes_env: false,
                home: None,
            }),
        },
        MachineRow {
            name: "gpu".into(),
            ssh: "gpu".into(),
            working_directory: None,
            status: MachineStatus::Disconnected,
            latency_ms: None,
            executor: None,
        },
    ];
    let lines = text(&app, 100);
    let heading = lines
        .iter()
        .find(|l| l.starts_with("   Machines"))
        .expect("the machines heading");
    assert!(
        heading.ends_with("1 ready  ·  1 not connected"),
        "{heading}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "   ✓ devbox        ready  ·  42 ms  ·  linux arm64"),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "   – gpu           not connected")
    );
}

/// ⏎ 复制完整的会话 ID，状态栏右边说一句到下一个键；没有会话时 ⏎ 什么也不做、状态栏里没有它。
#[test]
fn enter_copies_the_session_id() {
    let mut main = started();
    let _ = main.slash("/status");
    assert_eq!(
        main.settings_key(key(KeyCode::Enter)),
        Some(Action::Copy(ID.into()))
    );
    assert!(keys(&mut main).ends_with("Copied the session ID"));
    let mut fresh = app();
    let _ = fresh.slash("/status");
    assert_eq!(fresh.settings_key(key(KeyCode::Enter)), None);
    assert!(!keys(&mut fresh).contains("copy id"));
}

/// 查看子代理时：写子代理的会话；不问事实与计数（它们是主会话的）。
#[test]
fn while_viewing_a_subagent_status_shows_the_subagent() {
    let mut app = started();
    titled(&mut app);
    app.snapshots.agents = AgentList {
        entries: vec![SubagentEntry {
            agent: AgentId::new("agent_a"),
            child: SessionId::new("sess_child"),
            agent_type: "Explore".into(),
            description: "Look around".into(),
            status: SubagentStatus::Running,
            background: true,
            waiting: false,
            elapsed_ms: 0,
            ended_ago_ms: None,
            version: 0,
            owner: None,
            activity: None,
            parked: false,
            machine: Default::default(),
            tool_use: None,
            started_at_ms: 0,
            ended_at_ms: None,
        }],
        at: Some(0),
        shown: 0,
    };
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: "Look around".into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view: Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
    });
    let _ = app.slash("/status");
    assert!(!app.session_facts_wanted());
    assert_eq!(app.context_wanted(), None);
    let lines = text(&app, 100);
    assert!(
        lines
            .iter()
            .any(|l| l == "   Subagent   Explore  ·  running"),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|l| l == "              sess_child"));
    assert!(
        lines
            .iter()
            .any(|l| l == "              started by Fix the footer spacing")
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "   Context is counted for the main session only.")
    );
    assert!(!lines.iter().any(|l| l.starts_with("   Directory")));
    assert_eq!(
        app.settings_key(key(KeyCode::Enter)),
        Some(Action::Copy("sess_child".into()))
    );
}

/// 自己加的 provider 写目录里的名字，不写它的 UUID；长的服务器名字把第二列推后（至多行宽的一半）；规则文件的种类对齐。
#[test]
fn custom_providers_and_long_names_read_well() {
    let mut app = started();
    app.memory_files = vec![
        MemoryFile {
            label: "User memory".into(),
            display: "~/.zcode/AGENTS.md".into(),
            path: "/Users/dev/.zcode/AGENTS.md".into(),
        },
        MemoryFile {
            label: "Project memory".into(),
            display: "./AGENTS.md".into(),
            path: "/Users/dev/project/AGENTS.md".into(),
        },
    ];
    app.session_facts_arrived(SessionFacts {
        mcp: vec![
            server("plugin:zotero:zotero", McpServerState::Connected, 20, None),
            server("docs", McpServerState::Connected, 3, None),
        ],
        providers: vec![("fixture".into(), "My Gateway".into())],
        ..SessionFacts::default()
    });
    let lines = text(&app, 100);
    let has = |want: &str| {
        assert!(lines.iter().any(|l| l == want), "{want:?} in {lines:#?}");
    };
    has("   Model      scripted   My Gateway");
    has("   ✓ plugin:zotero:zotero  20 tools");
    has("   ✓ docs                   3 tools");
    has("     ~/.zcode/AGENTS.md   User memory");
    has("     ./AGENTS.md          Project memory");
    // 没有名字时不写 provider。
    app.session_facts_arrived(SessionFacts::default());
    assert!(
        text(&app, 100)
            .iter()
            .any(|l| l == "   Model      scripted"),
        "{:#?}",
        text(&app, 100)
    );
}
