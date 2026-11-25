//! 工作区信任的判定与回答的处理（docs/specs/rust-cli/workspace-trust.md §2.1、§2.5）：内存文件系统与内存存储，
//! 对话框用替身（记下收到的提示、给出预设的回答）。
use std::sync::Mutex;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::store::LocalSettings;
use rust_cli_kernel_api::testing::{MemoryEnvironment, MemoryStore};
use rust_cli_kernel_api::workspace_trust::TrustKey;
use rust_cli_tui_ports::trust::{TrustAnswer, TrustPrompt};

use super::*;

const HOME: &str = "/Users/dev";

fn env(files: &[(&str, &str)]) -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new("/"),
        platform: "darwin".to_owned(),
        os_version: "Darwin".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: Some(EnvPath::new(HOME)),
        uid: None,
    };
    files
        .iter()
        .fold(MemoryEnvironment::new(facts), |env, (path, text)| {
            env.with_file(path, text)
        })
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

/// 跑一次判定；`answer` 是对话框替身的回答。返回结果、对话框收到的提示与写下的记录。
async fn run(
    env: &MemoryEnvironment,
    store: &MemoryStore,
    root: &str,
    answer: TrustAnswer,
) -> (Outcome, Option<TrustPrompt>, Vec<String>) {
    let asked = Mutex::new(None);
    let outcome = ensure(
        Inputs {
            info: env,
            files: env,
            store,

            workspace: &workspace(root),
        },
        async |prompt| {
            *asked.lock().unwrap() = Some(prompt);
            Ok(answer)
        },
    )
    .await
    .unwrap();
    let keys = store
        .trusted_keys()
        .iter()
        .map(|key| key.as_str().to_owned())
        .collect();
    (outcome, asked.into_inner().unwrap(), keys)
}

async fn trust(store: &MemoryStore, key: &str) {
    store.trust_workspace(&TrustKey::new(key)).await.unwrap();
}

#[tokio::test]
async fn an_untrusted_folder_asks_and_records_the_answer_at_the_folder() {
    let (env, store) = (env(&[]), MemoryStore::default());
    let (outcome, asked, keys) = run(&env, &store, "/w/a/b", TrustAnswer::Trusted).await;
    assert_eq!(outcome, Outcome::Continue);
    let asked = asked.expect("the dialog was shown");
    assert_eq!(asked.path, "/w/a/b");
    assert!(!asked.backstop);
    assert_eq!(keys, ["/w/a/b"]);
    // 记过之后不再问。
    let (outcome, asked, _) = run(&env, &store, "/w/a/b", TrustAnswer::Exit(1)).await;
    assert_eq!((outcome, asked), (Outcome::Continue, None));
}

#[tokio::test]
async fn declining_exits_with_the_dialog_code_and_records_nothing() {
    let (env, store) = (env(&[]), MemoryStore::default());
    for code in [0, 1] {
        let (outcome, _, keys) = run(&env, &store, "/w", TrustAnswer::Exit(code)).await;
        assert_eq!(outcome, Outcome::Exit(code));
        assert!(keys.is_empty());
    }
}

#[tokio::test]
async fn inside_a_repository_the_record_goes_to_the_git_root() {
    let env = env(&[("/w/repo/.git/HEAD", "ref: refs/heads/main\n")]);
    let store = MemoryStore::default();
    let (_, asked, keys) = run(&env, &store, "/w/repo/a/b", TrustAnswer::Trusted).await;
    assert!(asked.is_some());
    assert_eq!(keys, ["/w/repo"]);
}

#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_is_not_a_git_root() {
    // git 根只认目录与文件；FIFO 等别的种类继续往上（F58：以前任何种类都算）。
    let env = env(&[("/w/repo/.git/HEAD", "ref")]).with_special("/w/repo/a/.git");
    let store = MemoryStore::default();
    let (_, _, keys) = run(&env, &store, "/w/repo/a/b", TrustAnswer::Trusted).await;
    assert_eq!(keys, ["/w/repo"]);
}

#[tokio::test]
async fn a_parent_grant_counts_outside_repositories_only_up_to_the_git_root_inside() {
    // 不在仓库里：父目录的记录一直向上算。
    let (plain, store) = (env(&[]), MemoryStore::default());
    trust(&store, "/w").await;
    let (_, asked, _) = run(&plain, &store, "/w/a/b", TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    // 在仓库里：仓库外的父目录不算，仓库内的中间目录算（§2.1）。
    let repo = env(&[("/w/repo/.git/HEAD", "ref")]);
    let (_, asked, _) = run(&repo, &store, "/w/repo/a/b", TrustAnswer::Exit(1)).await;
    assert!(asked.is_some());
    trust(&store, "/w/repo/a").await;
    let (outcome, asked, _) = run(&repo, &store, "/w/repo/a/b", TrustAnswer::Exit(1)).await;
    assert_eq!((outcome, asked), (Outcome::Continue, None));
}

#[tokio::test]
async fn a_worktree_shares_the_trust_of_its_main_repository() {
    let env = env(&[
        ("/w/main/.git/HEAD", "ref"),
        ("/w/main/.git/worktrees/wt/commondir", "../..\n"),
        ("/w/main/.git/worktrees/wt/gitdir", "/w/trees/wt/.git\n"),
        ("/w/trees/wt/.git", "gitdir: /w/main/.git/worktrees/wt\n"),
    ]);
    let store = MemoryStore::default();
    // 在 worktree 里同意：记录写在主仓库根（项目路径是规范的 git 根）。
    let (_, asked, keys) = run(&env, &store, "/w/trees/wt/src", TrustAnswer::Trusted).await;
    assert!(asked.is_some());
    assert_eq!(keys, ["/w/main"]);
    // 主仓库根信任过：别的 worktree 与主仓库都不再问。
    let (_, asked, _) = run(&env, &store, "/w/trees/wt", TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    let (_, asked, _) = run(&env, &store, "/w/main/x", TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
}

#[tokio::test]
async fn a_git_file_that_does_not_point_back_is_its_own_root() {
    // `gitdir` 回指别处（或 commondir 不对）时规范根就是 git 根。
    let env = env(&[
        ("/w/main/.git/worktrees/wt/commondir", "../..\n"),
        ("/w/main/.git/worktrees/wt/gitdir", "/elsewhere/.git\n"),
        ("/w/trees/wt/.git", "gitdir: /w/main/.git/worktrees/wt\n"),
    ]);
    let store = MemoryStore::default();
    let (_, _, keys) = run(&env, &store, "/w/trees/wt", TrustAnswer::Trusted).await;
    assert_eq!(keys, ["/w/trees/wt"]);
}

#[tokio::test]
async fn the_home_folder_is_trusted_for_this_run_only() {
    let (env, store) = (env(&[]), MemoryStore::default());
    let (outcome, asked, keys) = run(&env, &store, HOME, TrustAnswer::Trusted).await;
    assert_eq!(outcome, Outcome::Continue);
    assert!(asked.is_some());
    assert!(keys.is_empty(), "{keys:?}");
    // 下一次还问；home 有（手工写的）记录时不问。
    let (_, asked, _) = run(&env, &store, HOME, TrustAnswer::Exit(1)).await;
    assert!(asked.is_some());
    trust(&store, HOME).await;
    let (_, asked, _) = run(&env, &store, HOME, TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    // home 下不在仓库里的子目录随 home 的记录。
    let (_, asked, _) = run(&env, &store, "/Users/dev/p", TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
}

/// 经父目录信任就够了，不再做后备询问（workspace-trust.md §2.5）：PROJECT-CONFIG 起项目文件不给授权（config.md §1.1）。未信任时的
/// 提示里也没有项目授权。
#[tokio::test]
async fn a_parent_grant_is_enough_and_the_prompt_has_no_project_grants() {
    let (env, store) = (
        env(&[(
            "/w/a/zcode.json",
            r#"{"permission": {"allowedTools": ["Bash"]}}"#,
        )]),
        MemoryStore::default(),
    );
    trust(&store, "/w").await;
    let (outcome, asked, keys) = run(&env, &store, "/w/a", TrustAnswer::Exit(1)).await;
    assert_eq!(outcome, Outcome::Continue);
    assert_eq!(asked, None);
    assert_eq!(keys, ["/w"]);
    let (_, asked, _) = run(&env, &store, "/x", TrustAnswer::Exit(1)).await;
    let asked = asked.unwrap();
    assert!(!asked.backstop);
    assert_eq!(asked.grants, None);
}

/// Windows 的环境（platform `win32`，home 是 `C:\Users\Dev`）。
fn windows_env() -> MemoryEnvironment {
    MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("C:\\"),
        platform: "win32".to_owned(),
        os_version: "Windows".to_owned(),
        shell: "pwsh".to_owned(),
        is_git_repo: false,
        home: Some(EnvPath::new("C:\\Users\\Dev")),
        uid: None,
    })
}

/// Windows 上同一个文件夹的几种写法（盘符的大小写、`\\?\` 前缀、末尾的分隔符、`/`）记下的与查的是同一个键：同意一次之后，
/// 换一种写法再启动也不问（2026-11-12，用户报告“windows 上每次打开都要授权当前目录”时的取证）。
#[tokio::test]
async fn windows_spellings_of_one_folder_share_the_record() {
    let (env, store) = (windows_env(), MemoryStore::default());
    let (_, asked, keys) = run(&env, &store, "C:\\Work\\Proj", TrustAnswer::Trusted).await;
    assert!(asked.is_some());
    assert_eq!(keys, ["c:/work/proj"]);
    for again in [
        "C:\\Work\\Proj",
        "c:\\work\\proj",
        "C:\\Work\\Proj\\",
        "\\\\?\\C:\\Work\\Proj",
        "C:/Work/Proj",
    ] {
        let (_, asked, _) = run(&env, &store, again, TrustAnswer::Exit(1)).await;
        assert_eq!(asked, None, "{again}");
    }
}

/// Windows 的终端（Windows Terminal、PowerShell、cmd）缺省在 home 里打开。home 的同意只在这一次启动有效、不写记录（§2.1），
/// 所以在 home 里每次启动都问，与盘符的写法无关。
#[tokio::test]
async fn on_windows_the_home_folder_asks_every_start() {
    let (env, store) = (windows_env(), MemoryStore::default());
    for root in ["C:\\Users\\Dev", "c:\\users\\dev\\"] {
        let (outcome, asked, keys) = run(&env, &store, root, TrustAnswer::Trusted).await;
        assert_eq!(outcome, Outcome::Continue);
        assert!(asked.is_some(), "{root}");
        assert!(keys.is_empty(), "{keys:?}");
    }
}
