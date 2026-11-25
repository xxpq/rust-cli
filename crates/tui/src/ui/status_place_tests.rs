//! 状态栏第 2 行（docs/specs/rust-cli/tui-display.md §2.6「第 2 行」）：目录、分支、增删行数与标题，窄时按次序走开。
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_git::WorkingTree;
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId};
use rust_cli_kernel_api::panels::SubagentStatus;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::Style;
use rust_cli_tui_kit::render::width::string_width;

use super::{WorkingTrees, row, shorten};
use crate::ui::agents::Viewing;
use crate::ui::app::App;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn transcript(root: &str) -> Transcript {
    Transcript::new(
        selection(),
        PermissionMode::Default,
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(root),
        },
    )
}

/// 工作区在 `root`、家目录 `/Users/dev`、标题 `title`（空时没有标题）的 App。
fn app(root: &str, title: &str) -> App {
    let mut app = App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))).with_foreground(Some((255, 255, 255))),
        Lang::En,
        transcript(root),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    if !title.is_empty() {
        app.view.apply(
            SequencedEvent {
                seq: 1,
                revision: 0,
                event: KernelEvent::TitleChanged {
                    title: title.into(),
                },
            },
            0,
        );
    }
    app
}

/// runner 收下的工作树（同 `runner/working_tree.rs` 的一问一答）。
fn answer(app: &mut App, branch: Option<&str>, added: u64, removed: u64) {
    let root = app.workspace().to_owned();
    let seq = app.working_tree.begin(&root);
    let tree = WorkingTree {
        branch: branch.map(str::to_owned),
        added,
        removed,
    };
    assert!(app.working_tree.arrived(seq, root, Some(tree)));
}

fn plain(app: &App, width: usize) -> String {
    row(app, width).plain().trim_end().to_owned()
}

fn style_of(app: &App, width: usize, text: &str) -> Style {
    row(app, width)
        .spans
        .iter()
        .find(|span| span.text == text)
        .unwrap_or_else(|| panic!("{text:?} in {:?}", plain(app, width)))
        .style
}

#[test]
fn the_row_has_the_folder_the_branch_the_lines_and_the_title() {
    let mut app = app("/Users/dev/project", "Fix lint");
    answer(&mut app, Some("feat/x"), 12, 3);
    let line = plain(&app, 60);
    let left = "   ~/project · feat/x +12 −3";
    // 标题停在右沿（宽度 − 1）之前，同第 1 行。
    assert_eq!(
        line,
        format!("{left}{}Fix lint", " ".repeat(59 - string_width(left) - 8))
    );
    let theme = app.ink.theme;
    assert_eq!(style_of(&app, 60, "~/project"), theme.fg3);
    assert_eq!(style_of(&app, 60, "feat/x"), theme.fg3);
    assert_eq!(style_of(&app, 60, " · "), theme.line);
    assert_eq!(style_of(&app, 60, "+12"), Style::fg(theme.ok));
    assert_eq!(style_of(&app, 60, "−3"), Style::fg(theme.err));
    assert_eq!(style_of(&app, 60, "Fix lint"), theme.fg3);
}

/// 一边为 0 时不写那一边；都为 0 时没有行数；分离的 HEAD 只有行数；没有回执（还没问、内核不支持、不是仓库）时只有目录。
#[test]
fn missing_parts_are_left_out() {
    let mut app = app("/Users/dev/project", "");
    assert_eq!(plain(&app, 60), "   ~/project");
    answer(&mut app, Some("main"), 0, 0);
    assert_eq!(plain(&app, 60), "   ~/project · main");
    answer(&mut app, Some("main"), 4, 0);
    assert_eq!(plain(&app, 60), "   ~/project · main +4");
    answer(&mut app, Some("main"), 0, 2);
    assert_eq!(plain(&app, 60), "   ~/project · main −2");
    answer(&mut app, None, 4, 2);
    assert_eq!(plain(&app, 60), "   ~/project · +4 −2");
    answer(&mut app, None, 0, 0);
    assert_eq!(plain(&app, 60), "   ~/project");
}

/// 窄时的次序（§2.6）：先丢行数，再缩目录，再丢标题，再丢分支，目录最后走（最后截掉）。
#[test]
fn narrow_rows_drop_the_parts_in_order() {
    let mut app = app("/Users/dev/code/work/z-code-3", "Fix the status bar");
    answer(&mut app, Some("feat/rust-cli"), 181, 31);
    let full = "   ~/code/work/z-code-3 · feat/rust-cli +181 −31";
    assert_eq!(
        plain(&app, 80),
        format!("{full}{}Fix the status bar", " ".repeat(79 - 48 - 18))
    );
    // 标题先截到 TITLE_MIN 列，再丢行数。
    assert_eq!(plain(&app, 62), format!("{full}   Fix the s…"));
    let branch = "   ~/code/work/z-code-3 · feat/rust-cli";
    assert_eq!(
        plain(&app, 61),
        format!("{branch}{}Fix the status bar", " ".repeat(60 - 39 - 18))
    );
    assert_eq!(plain(&app, 53), format!("{branch}   Fix the s…"));
    let short = "   ~/…/z-code-3 · feat/rust-cli";
    assert_eq!(plain(&app, 52), format!("{short}   Fix the status b…"));
    assert_eq!(plain(&app, 45), format!("{short}   Fix the s…"));
    assert_eq!(plain(&app, 44), short);
    assert_eq!(plain(&app, 32), short);
    assert_eq!(plain(&app, 31), "   ~/…/z-code-3");
    assert_eq!(plain(&app, 16), "   ~/…/z-code-3");
    let tiny = plain(&app, 10);
    assert!(string_width(&tiny) <= 9 && tiny.ends_with('…'), "{tiny:?}");
}

/// 查看子代理：右边是它的描述（子代理的墨），分支走了之后还留着（tui-tasks.md §2.3 的 `stays`）。
#[test]
fn a_viewed_subagents_description_outlasts_the_branch() {
    let mut app = app("/Users/dev/project", "Fix lint");
    answer(&mut app, Some("feat/rust-cli"), 0, 0);
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: "Find   the callers".into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view: transcript("/Users/dev/project"),
    });
    assert!(plain(&app, 80).ends_with("Find the callers"));
    assert_eq!(
        style_of(&app, 80, "Find the callers"),
        app.ink.subagent_ink()
    );
    // 宽 20：分支走了，描述截到剩下的列。
    assert_eq!(plain(&app, 20), "   ~/project   Fin…");
}

/// 翻输入历史时右边是 `History k/n`。
#[test]
fn browsing_history_shows_the_position() {
    let mut app = app("/Users/dev/project", "Fix lint");
    for text in ["first", "second"] {
        app.composer.area.set_text(text);
        app.composer.discard();
    }
    assert!(app.composer.history_prev());
    assert!(plain(&app, 60).ends_with("History 1/2"));
}

/// 回执属于问的时候的根：根换了旧的不画；晚到的旧回执不覆盖新的。
#[test]
fn trees_belong_to_their_root_and_old_answers_are_dropped() {
    let mut trees = WorkingTrees::default();
    let tree = |branch: &str| WorkingTree {
        branch: Some(branch.to_owned()),
        added: 0,
        removed: 0,
    };
    assert!(trees.stale("/a"));
    let first = trees.begin("/a");
    assert!(!trees.stale("/a"));
    let second = trees.begin("/a");
    assert!(trees.arrived(second, "/a".into(), Some(tree("new"))));
    assert!(!trees.arrived(first, "/a".into(), Some(tree("old"))));
    assert_eq!(
        trees.of("/a").and_then(|t| t.branch.as_deref()),
        Some("new")
    );
    assert!(trees.stale("/b"));
    assert_eq!(trees.of("/b"), None);
    // 内核不支持：没有分支与行数。
    let third = trees.begin("/a");
    assert!(trees.arrived(third, "/a".into(), None));
    assert_eq!(trees.of("/a"), None);
}

#[test]
fn folders_shorten_to_the_first_part_and_the_last_folder() {
    assert_eq!(shorten("~/code/work/z-code-3"), "~/…/z-code-3");
    assert_eq!(shorten("/Users/dev/x"), "/…/x");
    assert_eq!(shorten(r"C:\Users\dev\x"), r"C:\…\x");
    assert_eq!(shorten("~/a/b"), "~/a/b");
    assert_eq!(shorten("~"), "~");
}
