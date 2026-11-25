//! 状态栏第 2 行（docs/specs/rust-cli/tui-display.md §2.6「第 2 行」）：目录、分支与未提交的增删行数来自执行环境（分支读
//! `.git/HEAD`，行数是 `git diff HEAD --numstat` 的合计），回合结束后重问。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::env_git::GitDiff;
use rust_cli_kernel_api::testing::{MemoryEnvironment, environments, text_response};

use super::support::{GatedModel, Setup, with_ui};

const HEAD: &str = "/Users/dev/project/.git/HEAD";

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: true,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    }
}

fn last(screen: &str) -> &str {
    screen.lines().last().unwrap_or("").trim_end()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_second_row_shows_the_branch_and_the_changed_lines() {
    // 两个文件改了 3+2 行、删了 1 行；二进制文件（`-`）不算。
    let env = Arc::new(
        MemoryEnvironment::new(facts())
            .with_file(HEAD, "ref: refs/heads/feat/x\n")
            .with_git_diff(GitDiff::Repository {
                numstat: "3\t1\tsrc/a.rs\n2\t0\tsrc/b.rs\n-\t-\timg.png".into(),
                patch: String::new(),
                untracked: String::new(),
            }),
    );
    let setup = Setup {
        width: 100,
        height: 30,
        environments: environments(env.clone()),
        ..Setup::default()
    };
    let model = GatedModel::scripted(vec![text_response(&["done"])]);
    with_ui(model, setup, move |mut ui| async move {
        let screen = ui
            .wait("the branch and the lines", |s| {
                last(s) == "   ~/project · feat/x +5 −1"
            })
            .await;
        // 第 1 行照旧是模式（倒数第 2 行）。
        assert!(
            super::support::status_row(&screen).starts_with("   Ask before changes"),
            "{screen}"
        );
        // 回合里换了分支：回合结束（TurnFinished）后重问，不等别的事。
        env.set_file(HEAD, "ref: refs/heads/feat/y\n");
        ui.submit("hello");
        ui.wait("the new branch after the turn", |s| {
            last(s).starts_with("   ~/project · feat/y +5 −1")
        })
        .await;
    })
    .await;
}
