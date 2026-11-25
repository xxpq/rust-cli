//! 读端慢的提问（缺陷 D2，docs/specs/rust-cli/session-lifecycle.md §2.1）：输出写不进去的时候会话照常往下跑，订阅流
//! 落后到实时广播装不下而结束；提问要从最后收到的 seq 接着订阅，照常写完这个回合。

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{CreateSession, KernelEvent};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
    selection, text_response,
};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use serde_json::Value;
use tokio::io::AsyncWrite;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::format::{OutputFormat, PrintFormat};
use crate::printer::Output;

/// 流式增量的片数：是会话日志实时广播缓冲（1024）的几倍，读端停着时一定落后。
const PIECES: usize = 3 * 1024;

/// 闸门打开之前写不进去的输出。
#[derive(Default)]
struct Gate {
    open: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Gate {
    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

struct Gated {
    gate: Arc<Gate>,
    written: Vec<u8>,
}

impl AsyncWrite for Gated {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if !self.gate.open.load(Ordering::SeqCst) {
            *self.gate.waker.lock().unwrap() = Some(cx.waker().clone());
            if !self.gate.open.load(Ordering::SeqCst) {
                return Poll::Pending;
            }
        }
        self.written.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn kernel(model: ScriptedModel) -> Arc<dyn Kernel> {
    let store = Arc::new(MemoryStore::default());
    Arc::new(LocalKernel::new(
        Ports {
            model: Arc::new(model),
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(0)),
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
    ))
}

/// 等这个会话的回合结束（自己的订阅流落后时同样接着订阅）。
async fn turn_finished(session: &Arc<dyn SessionHandle>) {
    let mut last = 0;
    loop {
        let mut events = session.subscribe(Some(last));
        while let Some(event) = events.next().await {
            last = event.seq;
            if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                return;
            }
        }
    }
}

#[tokio::test]
async fn a_slow_reader_resubscribes_and_writes_the_whole_turn() {
    let pieces: Vec<String> = (0..PIECES).map(|at| format!("{} ", at % 10)).collect();
    let chunks: Vec<&str> = pieces.iter().map(String::as_str).collect();
    let kernel = kernel(ScriptedModel::new(vec![text_response(&chunks)]));
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/work"),
            },
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let gate = Arc::new(Gate::default());
    let mut out = Gated {
        gate: gate.clone(),
        written: Vec::new(),
    };
    let output = Output {
        format: PrintFormat {
            output: OutputFormat::StreamJson,
            verbose: true,
            partial: false,
        },
        ..Output::default()
    };
    let asked = {
        let mut printer = Printer::new(output, &mut out);
        let mut driver = Driver {
            kernel: kernel.clone(),
            session: session.clone(),
            printer: &mut printer,
            stop: CancellationToken::new(),
            cancel: CancellationToken::new(),
            input_closed: true,
            ids: crate::test_ids(),
        };
        // system/init 写不进去的时候回合跑完：提问的订阅流在这期间落后。
        let release = async {
            turn_finished(&session).await;
            gate.open();
        };
        let (asked, ()) = tokio::join!(driver.ask("hi".to_owned()), release);
        asked
    };
    assert_eq!(asked.unwrap(), Asked::Done);
    let events: Vec<Value> = String::from_utf8(out.written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = events.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["result"], pieces.concat());
}

/// 真的会话，只换掉一处：权限询问由“hook”先答（转给会话），`-p` 的自动回答随后到，得到 owner 的 `NotFound`（已回答）。
struct HookAnswersFirst(Arc<dyn SessionHandle>);

#[async_trait::async_trait]
impl rust_cli_kernel_api::testing::HandleOverride for HookAnswersFirst {
    fn id(&self) -> &rust_cli_kernel_api::ids::SessionId {
        self.0.id()
    }

    async fn send(&self, command: CommandEnvelope) -> Ack {
        if !matches!(command.command, Command::ResolveInteraction { .. }) {
            return self.0.send(command).await;
        }
        // hook 替用户回答：先于这条自动回答到 owner。
        let ack = self.0.send(command).await;
        assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
        Ack::Rejected {
            refusal: rust_cli_kernel_api::admission::Refusal::NotFound,
            reason: "no pending interaction".to_owned(),
        }
    }

    fn subscribe(
        &self,
        after: Option<u64>,
    ) -> futures_util::stream::BoxStream<'static, rust_cli_kernel_api::session_api::SequencedEvent>
    {
        self.0.subscribe(after)
    }

    async fn view(
        &self,
    ) -> Result<
        rust_cli_kernel_api::session_api::SessionView,
        rust_cli_kernel_api::session_api::KernelError,
    > {
        self.0.view().await
    }
}

/// 修 bug（工作线 HOOKS-2，hooks.md §6.2）：PermissionRequest hook 与询问竞速（Q18），hook 先替答时 `-p` 的自动回答迟到、被拒为
/// `NotFound`。以前当成错误，`-p` 以 “the interaction could not be answered” 退出；现在照常写完这个回合。
#[tokio::test]
async fn a_late_automatic_answer_is_not_an_error() {
    let kernel = kernel(ScriptedModel::new(vec![
        rust_cli_kernel_api::testing::tool_use_response(
            "toolu_1",
            "Bash",
            &serde_json::json!({"command": "touch x"}),
        ),
        text_response(&["answered by the hook"]),
    ]));
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/work"),
            },
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let session: Arc<dyn SessionHandle> =
        Arc::new(HookAnswersFirst(kernel.open_session(&id).await.unwrap()));
    let mut out = Vec::new();
    let output = Output {
        format: PrintFormat {
            output: OutputFormat::StreamJson,
            verbose: true,
            partial: false,
        },
        ..Output::default()
    };
    let asked = {
        let mut printer = Printer::new(output, &mut out);
        let mut driver = Driver {
            kernel: kernel.clone(),
            session,
            printer: &mut printer,
            stop: CancellationToken::new(),
            cancel: CancellationToken::new(),
            input_closed: true,
            ids: crate::test_ids(),
        };
        driver.ask("hi".to_owned()).await
    };
    assert_eq!(asked.unwrap(), Asked::Done);
    let events: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = events.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["result"], "answered by the hook");
}
