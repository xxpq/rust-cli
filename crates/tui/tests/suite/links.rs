//! 别处的网址也能点（docs/specs/rust-cli/tui-select.md §10「别处的网址也能点」，用户决定 2026-11-12）：真实的回合里，你发的
//! 消息与工具调用里的网址单击就用打开方式打开（工具输出里的网址见单元测试 `ui/link_anywhere_tests.rs`：执行环境的替身
//! 不跑 Bash）。修复前只有回答里的链接能点。
use std::sync::{Arc, Mutex};

use crossterm::event::{MouseButton, MouseEventKind};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, environments, text_response, tool_use_response,
};
use rust_cli_tui::services::Opener;
use serde_json::json;

use super::support::{GatedModel, Setup, Ui, with_ui};

const ASKED: &str = "https://example.com/issue/7";
const PRINTED: &str = "https://ci.example.com/run/42";

/// 记下打开的网址。
#[derive(Default)]
struct Opened(Mutex<Vec<String>>);

impl Opener for Opened {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    }
}

/// `needle` 第一次出现处往右 2 格的（列，行）。
fn at(screen: &str, needle: &str) -> (u16, u16) {
    let (y, line) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} in\n{screen}"));
    let x = line[..line.find(needle).unwrap()].chars().count() + 2;
    (x as u16, y as u16)
}

fn click(ui: &Ui, (x, y): (u16, u16)) {
    ui.mouse(MouseEventKind::Down(MouseButton::Left), x, y);
    ui.mouse(MouseEventKind::Up(MouseButton::Left), x, y);
}

#[tokio::test(flavor = "multi_thread")]
async fn urls_in_your_message_and_in_a_tool_call_open_on_a_click() {
    let opened = Arc::new(Opened::default());
    let mut setup = Setup {
        width: 100,
        height: 40,
        environments: environments(Arc::new(MemoryEnvironment::new(facts()))),
        // 完全访问：Bash 不问（执行环境的替身给出执行错误，工具行照样写命令）。
        mode: PermissionMode::BypassPermissions,
        ..Setup::default()
    };
    setup.services.opener = Some(opened.clone());
    let model = GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &json!({"command": format!("curl -s {PRINTED}")}),
        ),
        text_response(&["The run is green."]),
    ]);
    let seen = opened.clone();
    with_ui(model, setup, move |mut ui| async move {
        ui.submit(&format!("look at {ASKED} please"));
        let folded = ui.wait_for("The run is green.").await;
        // 失败的调用收起时只写 `✗ Bash failed`：点它展开，看得见命令。
        click(&ui, at(&folded, "Bash failed"));
        let screen = ui.wait_for(PRINTED).await;
        click(&ui, at(&screen, ASKED));
        click(&ui, at(&screen, PRINTED));
        ui.wait("both urls to open", |_| seen.0.lock().unwrap().len() == 2)
            .await;
    })
    .await;
    assert_eq!(opened.0.lock().unwrap().as_slice(), [ASKED, PRINTED]);
}

/// ⌃t 读的会话页里的网址（tui-select.md §10「页里的网址」，用户决定 2026-11-12）：你的消息里的网址单击就用打开方式打开；
/// 回答的行内代码里的网址不是链接（Markdown 的规则），点了不开。修复前页里的网址都不能点。
#[tokio::test(flavor = "multi_thread")]
async fn urls_on_the_read_session_page_open_on_a_click() {
    use crossterm::event::{KeyCode, KeyModifiers};
    use rust_cli_kernel_api::testing::MemoryStore;

    use super::sessions_seed::{Seed, seed};

    const IN_CODE: &str = "https://example.com/code";
    let store = Arc::new(MemoryStore::default());
    seed(
        &store,
        &[Seed {
            id: "sess_link",
            root: "/Users/dev/project",
            title: "Link task",
            prompt: "open https://example.com/issue/9 please",
            reply: "run `curl https://example.com/code` first",
            created: 1_000_000,
            updated: 2_000_000,
        }],
    )
    .await;
    let opened = Arc::new(Opened::default());
    let mut setup = Setup {
        width: 100,
        height: 30,
        store,
        ..Setup::default()
    };
    setup.services.opener = Some(opened.clone());
    let seen = opened.clone();
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        move |mut ui| async move {
            ui.submit("/resume");
            ui.wait_for("Link task").await;
            ui.press(KeyCode::Char('t'), KeyModifiers::CONTROL);
            let screen = ui.wait_for("Sessions  ›  Link task").await;
            let screen = if screen.contains("first") {
                screen
            } else {
                ui.wait_for("first").await
            };
            click(&ui, at(&screen, IN_CODE));
            click(&ui, at(&screen, "https://example.com/issue/9"));
            ui.wait("the url to open", |_| !seen.0.lock().unwrap().is_empty())
                .await;
        },
    )
    .await;
    assert_eq!(
        opened.0.lock().unwrap().as_slice(),
        ["https://example.com/issue/9"]
    );
}
