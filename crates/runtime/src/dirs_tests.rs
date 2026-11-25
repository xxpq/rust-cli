//! `/add-dir` 的路径检查与加进会话（docs/specs/rust-cli/session-dirs.md §5）。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{CreateSession, Persistence};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, environments,
    selection,
};
use rust_cli_kernel_api::user_command::LOCAL_COMMAND_OUTPUT_SOURCE;

use super::*;
use crate::{Defaults, Ports};

fn kernel() -> LocalKernel {
    kernel_with(Arc::new(MemoryStore::default()))
}

fn kernel_with(store: Arc<MemoryStore>) -> LocalKernel {
    let env = Arc::new(
        MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".to_owned(),
            os_version: "x".to_owned(),
            shell: "zsh".to_owned(),
            is_git_repo: false,
            home: Some(EnvPath::new("/Users/dev")),
            uid: None,
        })
        .with_file("/Users/dev/project/src/a.rs", "a")
        .with_file("/Users/dev/other/b.rs", "b")
        .with_file("/Users/dev/notes.txt", "x"),
    );
    LocalKernel::new(
        Ports {
            model: Arc::new(ScriptedModel::new(Vec::new())),
            environments: environments(env),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
            additional_directories: Vec::new(),
        },
    )
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

fn request(path: &str, session: Option<SessionId>, drafts: &[&str]) -> AddDirectory {
    AddDirectory {
        workspace: workspace(),
        session,
        path: path.to_owned(),
        drafts: drafts.iter().map(|dir| EnvPath::new(*dir)).collect(),
        remember: false,
    }
}

fn remembering(request: AddDirectory) -> AddDirectory {
    AddDirectory {
        remember: true,
        ..request
    }
}

async fn error(kernel: &LocalKernel, request: AddDirectory) -> String {
    kernel.add_directory(request).await.unwrap_err().to_string()
}

/// 还没有会话：只检查路径（相对工作区根，`~` 是 home）与草稿里的目录，不发命令。
#[tokio::test]
async fn without_a_session_the_path_is_checked_against_the_drafts() {
    let kernel = kernel();
    let added = kernel
        .add_directory(request(" ../other ", None, &[]))
        .await
        .unwrap();
    assert_eq!(added.directory.as_str(), "/Users/dev/other");
    assert_eq!(
        error(&kernel, request("../missing", None, &[])).await,
        "Path /Users/dev/missing was not found."
    );
    assert_eq!(
        error(&kernel, request("~/notes.txt", None, &[])).await,
        "~/notes.txt is not a directory. Did you mean to add the parent directory /Users/dev?"
    );
    assert_eq!(
        error(&kernel, request("src", None, &[])).await,
        "src is already accessible within the current working directory /Users/dev/project."
    );
    assert_eq!(
        error(&kernel, request("~/other", None, &["/Users/dev/other"])).await,
        "~/other is already added as a working directory."
    );
}

/// 有会话：加进会话（actor 判定），用不了的路径也记进对话。
#[tokio::test]
async fn with_a_session_the_actor_adds_and_records() {
    let kernel = kernel();
    let session = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let added = kernel
        .add_directory(request("../other", Some(session.clone()), &[]))
        .await
        .unwrap();
    assert_eq!(added.directory.as_str(), "/Users/dev/other");
    // 草稿不看：判定在 actor，它已经有这个目录。
    assert_eq!(
        error(&kernel, request("../other", Some(session.clone()), &[])).await,
        "../other is already added as a working directory."
    );
    assert_eq!(
        error(&kernel, request("../missing", Some(session.clone()), &[])).await,
        "Path /Users/dev/missing was not found."
    );
    let view = kernel
        .open_session(&session)
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    assert_eq!(
        view.additional_directories,
        [EnvPath::new("/Users/dev/other")]
    );
    let outputs: Vec<String> = view
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user)
                if matches!(&user.origin, UserOrigin::Meta { source } if source == LOCAL_COMMAND_OUTPUT_SOURCE) =>
            {
                match user.content.as_slice() {
                    [ContentBlock::Text { text }] => Some(text.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 3, "{outputs:?}");
    assert!(outputs[0].contains("as a working directory for this session"));
    assert!(outputs[1].contains("is already added as a working directory"));
    assert!(outputs[2].contains("was not found."));
}

/// 确认框之前的检查（permission-updates.md §6.1）：路径与是否已在工作目录里都在这里判，能加时给出绝对路径、不加进去；有会话时按会话
/// 现在的目录判，用不了的也记进对话。
#[tokio::test]
async fn the_check_before_the_dialog_does_not_add() {
    let kernel = kernel();
    let checked = kernel
        .check_directory(request("../other", None, &[]))
        .await
        .unwrap();
    assert_eq!(checked.directory.as_str(), "/Users/dev/other");
    assert_eq!(
        kernel
            .check_directory(request("~/other", None, &["/Users/dev/other"]))
            .await
            .unwrap_err()
            .to_string(),
        "~/other is already added as a working directory."
    );
    let session = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: Persistence::Immediate,
            additional_directories: vec![EnvPath::new("/Users/dev/other")],
            preferences: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        kernel
            .check_directory(request("../other", Some(session.clone()), &[]))
            .await
            .unwrap_err()
            .to_string(),
        "../other is already added as a working directory."
    );
    let view = kernel
        .open_session(&session)
        .await
        .unwrap()
        .view()
        .await
        .unwrap();
    // 只检查：会话的目录不变，「已在工作目录里」照旧记进对话。
    assert_eq!(
        view.additional_directories,
        [EnvPath::new("/Users/dev/other")]
    );
    assert!(
        view.messages
            .iter()
            .any(|message| matches!(message, Message::User(user) if matches!(&user.origin, UserOrigin::Meta { source } if source == LOCAL_COMMAND_OUTPUT_SOURCE))),
        "{:?}",
        view.messages
    );
}

/// 「记住」：还没有会话时 runtime 直接存进本机项目规则集；有会话时经 actor（`DirectoriesRemembered`）。两种都回报存好了。
#[tokio::test]
async fn remembered_directories_reach_the_local_ruleset() {
    use rust_cli_kernel_api::store::LocalSettings;
    use rust_cli_kernel_api::working_dirs::Remembered;
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel_with(store.clone());
    let added = kernel
        .add_directory(remembering(request("../other", None, &[])))
        .await
        .unwrap();
    assert_eq!(added.remembered, Remembered::Saved);
    let session = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let added = kernel
        .add_directory(remembering(request("~", Some(session), &[])))
        .await
        .unwrap();
    assert_eq!(added.remembered, Remembered::Saved);
    assert_eq!(
        store
            .project_rules(&workspace())
            .await
            .unwrap()
            .additional_directories,
        [EnvPath::new("/Users/dev/other"), EnvPath::new("/Users/dev")]
    );
}
