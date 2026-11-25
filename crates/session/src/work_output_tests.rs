//! 后台命令的状态与输出末尾（`SessionHandle::work_output`，session-lifecycle.md §10.44）：状态取会话的后台工作表——在跑、终态通知之后
//! （执行环境的账本已经忘了它）、停掉之后都有；末尾从执行环境的输出文件只读最后的字节（`Files::read_tail`，文件多大都能读，
//! session-lifecycle.md §10.44），读不到时说明原因。

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::background::{BackgroundChange, OutputTail, OutputUnreadable, WorkOutput};
use rust_cli_kernel_api::env::{
    EnvError, EnvPath, FileContent, OperationContext, ReadFile, WorkspaceRef, WriteFile,
    WriteReceipt,
};
use rust_cli_kernel_api::env_files::{FileTail, ReadTail};
use rust_cli_kernel_api::env_shell::{BackgroundStatus, ShellNotice, ShellState};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::machine::{Files, Shell};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::testing::{
    FakeTool, NoticeEnvironment, ScriptedModel, machine, shell_notice, text_response,
    tool_use_response,
};
use serde_json::json;

use crate::test_support::{Setup, finished, submit, until};

const OUTPUTS: &str = "/tmp/tasks";
/// `b1` 的输出：多字节的字符跨在末尾的切口上。
const OUTPUT: &str = "first line\n第二行\n";
const TAIL_BYTES: usize = 8;

/// 执行环境：通知交给 [`NoticeEnvironment`]，shell 状态带输出目录，输出文件按路径给出（字节，或读的错误）。
struct Outputs {
    notices: Arc<NoticeEnvironment>,
    files: BTreeMap<String, Result<Vec<u8>, EnvError>>,
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "test".to_owned(),
    }
}

fn path(task: &str) -> String {
    format!("{OUTPUTS}/{task}.output")
}

/// `b2` 的输出：超过以前的 1 MiB 上限，末尾是最后一行。
fn big_output() -> Vec<u8> {
    let mut bytes = vec![b'x'; 2 << 20];
    bytes.extend_from_slice(b"END\n");
    bytes
}

impl Outputs {
    fn new(notices: Arc<NoticeEnvironment>) -> Arc<Self> {
        let files = BTreeMap::from([
            (path("b1"), Ok(OUTPUT.as_bytes().to_vec())),
            (path("b2"), Ok(big_output())),
            (
                path("b4"),
                Err(EnvError::Io {
                    message: "is a directory".to_owned(),
                }),
            ),
        ]);
        Arc::new(Self { notices, files })
    }
}

#[async_trait]
impl Files for Outputs {
    async fn read_file(&self, _w: &WorkspaceRef, _r: ReadFile) -> Result<FileContent, EnvError> {
        Err(unused())
    }

    async fn write_file(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(unused())
    }

    async fn read_tail(&self, _w: &WorkspaceRef, request: ReadTail) -> Result<FileTail, EnvError> {
        let path = request.path.as_str().to_owned();
        let bytes = self
            .files
            .get(&path)
            .cloned()
            .unwrap_or(Err(EnvError::NotFound { path }))?;
        let length = bytes
            .len()
            .min(usize::try_from(request.max_bytes).unwrap_or(usize::MAX));
        Ok(FileTail {
            total_bytes: bytes.len() as u64,
            bytes: bytes[bytes.len() - length..].to_vec(),
        })
    }
}

#[async_trait]
impl Shell for Outputs {
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        let state = self.notices.shell_state(workspace, session).await?;
        Ok(ShellState {
            outputs: Some(EnvPath::new(OUTPUTS)),
            ..state
        })
    }

    fn shell_notices(
        &self,
        session: &SessionId,
        after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        self.notices.shell_notices(session, after)
    }

    async fn ack_shell_notice(&self, session: &SessionId, seq: u64) -> Result<(), EnvError> {
        self.notices.ack_shell_notice(session, seq).await
    }
}

/// 调用之后报告起了 `change` 的工具（替 Bash、Monitor、TaskStop）。
fn tool(name: &str, change: BackgroundChange) -> Arc<FakeTool> {
    let mut tool = FakeTool::new(name, true, "done");
    tool.output = change.attach(tool.output);
    Arc::new(tool)
}

fn launched(id: &str, kind: BackgroundKind) -> BackgroundChange {
    BackgroundChange::Launched {
        id: id.to_owned(),
        kind,
        description: format!("run {id}"),
        child: None,
    }
}

/// 起会话：模型依次调 `tools` 里的每个工具（各一轮工具调用），最后回一句话。
async fn session_with(
    notices: Arc<NoticeEnvironment>,
    tools: Vec<Arc<FakeTool>>,
) -> crate::test_support::Built {
    let mut replies: Vec<_> = tools
        .iter()
        .enumerate()
        .map(|(index, tool)| tool_use_response(&format!("toolu_{index}"), &tool.name, &json!({})))
        .collect();
    replies.push(text_response(&["started"]));
    replies.push(text_response(&["noted"]));
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(replies)));
    let outputs = Outputs::new(notices.clone());
    setup.set_machine(
        machine(notices)
            .with_shell(outputs.clone())
            .with_files(outputs),
    );
    setup.tools = tools
        .into_iter()
        .map(|tool| tool as Arc<dyn rust_cli_kernel_api::tool::Tool>)
        .collect();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, finished).await;
    built
}

fn tail(status: BackgroundStatus) -> Option<WorkOutput> {
    Some(WorkOutput {
        status,
        read: Ok(OutputTail {
            path: EnvPath::new(path("b1")),
            // 末尾 8 字节切在「第」的中间：从下一个字符边界开始
            text: "二行\n".to_owned(),
            truncated: true,
        }),
    })
}

#[tokio::test]
async fn a_command_reports_its_tail_while_running_and_its_status_after_its_notice() {
    let notices = Arc::new(NoticeEnvironment::default());
    let built = session_with(
        notices.clone(),
        vec![tool("Launch", launched("b1", BackgroundKind::Shell))],
    )
    .await;
    let output = built.session.work_output("b1", TAIL_BYTES).await.unwrap();
    assert_eq!(output, tail(BackgroundStatus::Running));
    let mut events = built.session.subscribe(None);
    let mut notice = shell_notice(1, "b1");
    notice.exit_code = Some(2);
    notices.finish(notice);
    // 通知开了一轮（交给模型、ack 之后执行环境不再认识它）：状态取后台工作表记下的终态
    until(&mut events, finished).await;
    let output = built.session.work_output("b1", TAIL_BYTES).await.unwrap();
    assert_eq!(output, tail(BackgroundStatus::Failed));
}

#[tokio::test]
async fn a_stopped_command_is_killed() {
    let built = session_with(
        Arc::new(NoticeEnvironment::default()),
        vec![
            tool("Launch", launched("b1", BackgroundKind::Shell)),
            tool(
                "Stop",
                BackgroundChange::Stopped {
                    id: "b1".to_owned(),
                },
            ),
        ],
    )
    .await;
    let output = built.session.work_output("b1", TAIL_BYTES).await.unwrap();
    assert_eq!(output, tail(BackgroundStatus::Killed));
}

#[tokio::test]
async fn a_large_output_gives_its_last_bytes() {
    let built = session_with(
        Arc::new(NoticeEnvironment::default()),
        vec![tool("Big", launched("b2", BackgroundKind::Monitor))],
    )
    .await;
    // 以前按行整份读、超过 1 MiB 回 TooLarge；现在只读末尾（session-lifecycle.md §10.44）
    assert_eq!(
        built.session.work_output("b2", TAIL_BYTES).await.unwrap(),
        Some(WorkOutput {
            status: BackgroundStatus::Running,
            read: Ok(OutputTail {
                path: EnvPath::new(path("b2")),
                text: "xxxxEND\n".to_owned(),
                truncated: true,
            }),
        })
    );
}

#[tokio::test]
async fn unreadable_outputs_say_why_and_unknown_works_are_none() {
    let built = session_with(
        Arc::new(NoticeEnvironment::default()),
        vec![
            tool("Gone", launched("b3", BackgroundKind::Shell)),
            tool("Dir", launched("b4", BackgroundKind::Shell)),
            tool("Agent", launched("agent_1", BackgroundKind::Agent)),
        ],
    )
    .await;
    let read = |output: Option<WorkOutput>| output.map(|output| (output.status, output.read));
    assert_eq!(
        read(built.session.work_output("b3", TAIL_BYTES).await.unwrap()),
        Some((BackgroundStatus::Running, Err(OutputUnreadable::NotFound)))
    );
    assert_eq!(
        read(built.session.work_output("b4", TAIL_BYTES).await.unwrap()),
        Some((BackgroundStatus::Running, Err(OutputUnreadable::Io)))
    );
    // 子代理不是命令；不认识的 id 没有
    assert_eq!(
        built
            .session
            .work_output("agent_1", TAIL_BYTES)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        built.session.work_output("b9", TAIL_BYTES).await.unwrap(),
        None
    );
}
