//! 设置 › MCP 的服务器页上远程服务器的授权动作（tui-manage.md §6.5）：没有令牌时 Sign in；有令牌时 Sign in again 与 Sign out；
//! 要授权时没有 Reconnect；Sign in 关上设置、交给 runner；不用 OAuth 的服务器没有这些行。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::manage::McpAuthState;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::super::data::{self, McpState, Section, SectionData};
use super::super::page::{Change, Page, Row};
use super::super::{Request, Settings};
use crate::ui::app::{Action, App};
use crate::ui::i18n::Lang;
use crate::ui::manage::Manage;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn server(state: McpState, auth: Option<McpAuthState>) -> data::McpServer {
    data::McpServer {
        name: "docs".into(),
        enabled: true,
        state,
        auth,
        ..data::McpServer::default()
    }
}

/// 设置页开在 docs 这个服务器的页上。
fn app_on(server: data::McpServer) -> App {
    let mut app = App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    let mut ui = Settings {
        section: rust_cli_tui_kit::app::widgets::settings::section_index("mcp").unwrap_or(0),
        in_list: false,
        ..Settings::default()
    };
    ui.data.insert(Section::Mcp, SectionData::Mcp(vec![server]));
    ui.stack.push(Page::Mcp("docs".into()));
    app.settings = Some(ui);
    app
}

fn ids(app: &App) -> Vec<String> {
    let ui = app.settings.clone().expect("settings open");
    app.settings_view(&ui, 0).rows.iter().map(Row::id).collect()
}

fn press_on(app: &mut App, id: &str) -> Option<Action> {
    let ui = app.settings.as_mut().expect("settings open");
    ui.chosen.insert(ui.page(), id.to_owned());
    app.settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

#[test]
fn a_server_that_needs_authorization_offers_sign_in_and_no_reconnect() {
    let mut app = app_on(server(McpState::NeedsAuth, Some(McpAuthState::SignedOut)));
    let rows = ids(&app);
    assert!(rows.contains(&"signIn".to_owned()), "{rows:?}");
    assert!(!rows.contains(&"signOut".to_owned()), "{rows:?}");
    assert!(!rows.contains(&"reconnect".to_owned()), "{rows:?}");
    // Sign in 关上设置，交给 runner 去问内核
    let action = press_on(&mut app, "signIn");
    assert!(matches!(action, Some(Action::Manage(Manage::McpSignIn(name))) if name == "docs"));
    assert!(app.settings.is_none());
}

#[test]
fn a_signed_in_server_offers_sign_in_again_and_sign_out() {
    let mut app = app_on(server(McpState::Connected, Some(McpAuthState::SignedIn)));
    let rows = ids(&app);
    assert!(rows.contains(&"signIn".to_owned()), "{rows:?}");
    assert!(rows.contains(&"signOut".to_owned()), "{rows:?}");
    assert!(rows.contains(&"reconnect".to_owned()), "{rows:?}");
    let action = press_on(&mut app, "signOut");
    let Some(Action::Settings(Request::Write { write, .. })) = action else {
        panic!("expected a write, got {action:?}");
    };
    assert_eq!(
        write.change,
        Change::McpSignOut {
            name: "docs".into()
        }
    );
}

#[test]
fn a_server_without_oauth_has_no_sign_in_rows() {
    let app = app_on(server(McpState::Connected, None));
    let rows = ids(&app);
    assert!(
        !rows.iter().any(|id| id == "signIn" || id == "signOut"),
        "{rows:?}"
    );
}
