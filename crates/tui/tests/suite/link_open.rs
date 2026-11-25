//! 单击回答里的链接打不开时（docs/specs/rust-cli/tui-select.md §10，用户决定 2026-11-12）：输入框上方安静的一行说一句并写出
//! 网址。以前只记日志，点了没反应也不知道为什么。
use std::sync::Arc;

use crossterm::event::{MouseButton, MouseEventKind};
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, row_of, with_ui};

const URL: &str = "https://example.com/docs";

/// 起不了打开浏览器的程序（没有 `xdg-open` 一类）。
struct Broken;

impl rust_cli_tui::services::Opener for Broken {
    fn open(&self, _url: &str) -> Result<(), String> {
        Err("No such file or directory (os error 2)".to_owned())
    }
}

fn setup(lang: &'static str) -> Setup {
    let mut services = super::stub_services::services();
    services.opener = Some(Arc::new(Broken));
    Setup {
        width: 100,
        height: 30,
        lang,
        services,
        ..Setup::default()
    }
}

/// 链接的字在画面上的（列，行）。
fn at(screen: &str, text: &str) -> (u16, u16) {
    let row = row_of(screen, text).unwrap();
    let line = screen.lines().nth(usize::from(row)).unwrap();
    let col = line[..line.find(text).unwrap()].chars().count();
    (u16::try_from(col + 1).unwrap(), row)
}

async fn click_fails(lang: &'static str, said: &str) {
    let model = GatedModel::scripted(vec![text_response(&[&format!(
        "see [the docs]({URL}) now"
    )])]);
    let said = said.to_owned();
    with_ui(model, setup(lang), move |mut ui| async move {
        ui.submit("where are the docs");
        let screen = ui.wait_for("the docs now").await;
        let (x, y) = at(&screen, "the docs now");
        ui.mouse(MouseEventKind::Down(MouseButton::Left), x, y);
        ui.mouse(MouseEventKind::Up(MouseButton::Left), x, y);
        let screen = ui.wait_for(&said).await;
        // 在输入框上方（板的上沿之前）。
        let board = screen
            .lines()
            .position(|l| l.starts_with('▄') || l.starts_with('─'))
            .unwrap();
        assert!(
            usize::from(row_of(&screen, &said).unwrap()) < board,
            "{screen}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_that_cannot_be_opened_says_so_with_its_address() {
    click_fails("en_US.UTF-8", &format!("Couldn't open the link: {URL}")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn in_chinese_too() {
    click_fails("zh_CN.UTF-8", &format!("打不开链接：{URL}")).await;
}
