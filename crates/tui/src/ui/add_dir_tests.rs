//! `/add-dir` 的确认框（permission-updates.md §6.1）：三项与 esc。
use std::time::Instant;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::user_command::LocalCommandOutput;
use rust_cli_kernel_api::working_dirs;

use super::*;
use crate::ui::app::Overlay;
use crate::ui::test_support::{app, key};

fn opened() -> App {
    let mut app = app();
    app.ask_directory("../extra".to_owned(), EnvPath::new("/Users/dev/extra"));
    app
}

#[test]
fn the_dialog_lists_the_three_answers() {
    let app = opened();
    let Some(Overlay::List(list)) = &app.overlay else {
        panic!("the dialog is a list");
    };
    assert_eq!(list.title, "Add directory to workspace");
    assert_eq!(
        list.subtitle.as_deref(),
        Some(
            "/Users/dev/extra\nZCode will be able to read files in this directory and make edits when auto-accept edits is on."
        )
    );
    let labels: Vec<&str> = list.items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Yes, for this session",
            "Yes, and remember this directory",
            "No"
        ]
    );
}

#[test]
fn the_first_two_answers_add_the_directory() {
    let now = Instant::now();
    let mut app = opened();
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Manage(Manage::ConfirmDirectory {
            path: "../extra".to_owned(),
            remember: false,
        }))
    );
    assert!(app.overlay.is_none());
    let mut app = opened();
    app.on_key(key(KeyCode::Down), now);
    assert_eq!(
        app.on_key(key(KeyCode::Enter), now),
        Some(Action::Manage(Manage::ConfirmDirectory {
            path: "../extra".to_owned(),
            remember: true,
        }))
    );
}

/// 「不加」与 esc：不交给内核，说一句、记进对话。
#[test]
fn no_and_esc_decline_and_are_recorded() {
    let now = Instant::now();
    for keys in [vec![KeyCode::Char('3')], vec![KeyCode::Esc]] {
        let mut app = opened();
        let mut last = None;
        for code in keys {
            last = app.on_key(key(code), now);
        }
        assert_eq!(last, None);
        assert!(app.overlay.is_none());
        assert_eq!(app.records.len(), 1);
        assert_eq!(
            app.records[0].output,
            LocalCommandOutput::Stdout(working_dirs::declined_record("/Users/dev/extra"))
        );
        assert_eq!(app.records[0].args, "../extra");
    }
}
