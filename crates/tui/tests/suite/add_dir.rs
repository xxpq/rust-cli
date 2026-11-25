//! `/add-dir`（docs/specs/rust-cli/session-dirs.md §5）：加进会话的目录，下一个请求的记录与环境段带上它，default 模式下读那里的
//! 文件不询问；还没有会话时加进新会话的草稿；各种错误的文字见 session-dirs.md §5.3。能加时先弹确认框（只给这个会话 / 记住 /
//! 不加，permission-updates.md §6.1）。
use std::sync::Arc;

use crossterm::event::KeyCode;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, environments, text_response, tool_use_response,
};
use serde_json::json;

use super::support::{GatedModel, Setup, Ui, request_text, with_ui};

/// 确认框出来后选第 `choice` 项（1 只给这个会话、2 记住、3 不加）。
async fn answer(ui: &mut Ui, choice: char) {
    ui.wait_for("Yes, for this session").await;
    ui.key(KeyCode::Char(choice));
}

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(
        MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            home: Some(EnvPath::new("/Users/dev")),
            uid: None,
        })
        .with_file("/Users/dev/project/a.txt", "a")
        .with_file("/Users/dev/other/b.txt", "OTHER-CONTENT")
        .with_file("/Users/dev/notes.txt", "x"),
    )
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc stop")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_added_directory_reaches_the_next_request_and_the_permission_check() {
    let model = GatedModel::scripted(vec![
        text_response(&["first"]),
        tool_use_response(
            "toolu_1",
            "Read",
            &json!({"file_path": "/Users/dev/other/b.txt"}),
        ),
        text_response(&["done"]),
    ]);
    let setup = Setup {
        width: 120,
        environments: environments(env()),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("/add-dir");
        ui.wait_for("Usage: /add-dir <path>").await;
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        ui.submit("/add-dir ../missing");
        ui.wait_for("Path /Users/dev/missing was not found.").await;
        ui.submit("/add-dir ../other");
        answer(&mut ui, '1').await;
        ui.wait_for("Added /Users/dev/other as a working directory for this session")
            .await;
        ui.submit("/add-dir ~/other");
        ui.wait_for("~/other is already added as a working directory.")
            .await;
        // default 模式：附加目录是工作目录，读它不询问（没有询问框，回合自己走完）。
        ui.submit("read it");
        ui.wait("the last reply", |s| idle(s, "   done")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let second = request_text(&requests[1]);
    assert!(
        second.contains("<command-name>/add-dir</command-name>"),
        "{second}"
    );
    assert!(
        second.contains("as a working directory for this session"),
        "{second}"
    );
    assert!(
        second.contains("Additional working directories added:\\n  - /Users/dev/other"),
        "{second}"
    );
    assert!(!request_text(&requests[0]).contains("/Users/dev/other"));
    assert!(request_text(&requests[2]).contains("OTHER-CONTENT"));
}

/// 还没有会话时：加进新会话的草稿，第一个请求的环境段整块列出它，记录排在第一条输入之前。
#[tokio::test(flavor = "multi_thread")]
async fn a_directory_added_before_the_first_message_goes_with_the_new_session() {
    let model = GatedModel::scripted(vec![text_response(&["first"])]);
    let setup = Setup {
        width: 120,
        environments: environments(env()),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("/add-dir ../other");
        answer(&mut ui, '1').await;
        ui.wait_for("Added /Users/dev/other as a working directory for this session")
            .await;
        ui.submit("/add-dir ../other");
        ui.wait_for("../other is already added as a working directory.")
            .await;
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
    })
    .await;
    let requests = model.requests();
    let first = request_text(&requests[0]);
    assert!(
        first.contains("Additional working directories:\\n  - /Users/dev/other"),
        "{first}"
    );
    let record = first.find("<command-name>/add-dir</command-name>").unwrap();
    assert!(record < first.find("hello").unwrap(), "{first}");
}

/// `/clear` 之后的新会话带上上一个会话的附加工作目录（同工作区，session-dirs.md §5.4）：第一个请求的环境段整块列出它。
#[tokio::test(flavor = "multi_thread")]
async fn clear_carries_the_directories_to_the_new_session() {
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let setup = Setup {
        width: 120,
        environments: environments(env()),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        ui.submit("/add-dir ../other");
        answer(&mut ui, '1').await;
        ui.wait_for("Added /Users/dev/other as a working directory for this session")
            .await;
        ui.submit("/clear");
        ui.wait_for("Started a new session.").await;
        ui.submit("again");
        ui.wait("the second reply", |s| idle(s, "   second")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let second = request_text(&requests[1]);
    assert!(
        second.contains("Additional working directories:\\n  - /Users/dev/other"),
        "{second}"
    );
}

/// 「记住」：目录进本机项目规则集（下次启动作内核缺省），记录写 `and saved to local settings`；「不加」：不进会话，记录写
/// `Did not add …`。
#[tokio::test(flavor = "multi_thread")]
async fn remember_saves_the_directory_and_no_declines_it() {
    use rust_cli_kernel_api::store::LocalSettings;
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let setup = Setup {
        width: 120,
        environments: environments(env()),
        ..Setup::default()
    };
    let store = setup.store.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        ui.submit("/add-dir ../other");
        answer(&mut ui, '2').await;
        ui.wait_for("Added /Users/dev/other as a working directory and saved to local settings")
            .await;
        ui.submit("/add-dir ~");
        answer(&mut ui, '3').await;
        ui.wait_for("Did not add /Users/dev as a working directory.")
            .await;
        ui.submit("again");
        ui.wait("the second reply", |s| idle(s, "   second")).await;
    })
    .await;
    let requests = model.requests();
    let second = request_text(&requests[1]);
    assert!(
        second.contains("as a working directory and saved to local settings"),
        "{second}"
    );
    assert!(
        second.contains("Did not add \\u001b[1m/Users/dev\\u001b[22m as a working directory."),
        "{second}"
    );
    assert!(
        second.contains("Additional working directories added:\\n  - /Users/dev/other"),
        "{second}"
    );
    let workspace = rust_cli_kernel_api::env::WorkspaceRef {
        environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    };
    assert_eq!(
        store
            .project_rules(&workspace)
            .await
            .unwrap()
            .additional_directories,
        [EnvPath::new("/Users/dev/other")]
    );
}

/// `/fork` 出的会话带着父会话的附加工作目录（session-dirs.md §12.5，同一个进程里还在）：fork 之后读那里的文件不询问，环境段也不说
/// 它去掉了。以前 fork 出的会话只有内核缺省：读要询问，环境段宣布目录去掉。
#[tokio::test(flavor = "multi_thread")]
async fn a_forked_session_keeps_the_added_directories() {
    let model = GatedModel::scripted(vec![
        text_response(&["first"]),
        tool_use_response(
            "toolu_1",
            "Read",
            &json!({"file_path": "/Users/dev/other/b.txt"}),
        ),
        text_response(&["done"]),
    ]);
    let setup = Setup {
        width: 120,
        environments: environments(env()),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("/add-dir ../other");
        answer(&mut ui, '1').await;
        ui.wait_for("Added /Users/dev/other as a working directory for this session")
            .await;
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        ui.submit("/fork");
        ui.wait_for("a new session after this turn").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Switched to forked session").await;
        ui.submit("read it");
        ui.wait("the last reply", |s| idle(s, "   done")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let read = request_text(&requests[1]);
    assert!(
        !read.contains("Additional working directories removed:"),
        "{read}"
    );
    assert!(request_text(&requests[2]).contains("OTHER-CONTENT"));
}
