//! 第一条消息在 SessionStart hook 跑完之前（docs/specs/rust-cli/tui.md §5.1.2，2026-11-12 修复）：真实的内核组装，
//! hook 的命令一直不结束、直到测试放行。这期间第一条输入排在内核的队列里（`QueueChanged`，投递 `Queue`）。修复前它画在
//! 板上的栈里、槽写 `next turn`，hook 跑完开了回合才跳进对话，页面闪一下；修复后它一直画在对话的尾巴里。
use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvPath, OperationContext, Outcome, StartCommand, WorkspaceRef,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::machine::Commands;
use rust_cli_kernel_api::testing::{registry, text_response, unsupported_machine};
use rust_cli_runtime::{HookSource, HooksConfig, HooksSettings};
use tokio::sync::{Notify, watch};

use super::support::{GatedModel, Setup, with_ui};

/// hook 命令的命令端口：第一条命令（SessionStart）一直不结束，直到测试放行（`release`）；之后的命令立即以 0 结束。
struct HeldStart {
    first: Mutex<Option<OperationId>>,
    started: Notify,
    release: watch::Sender<bool>,
}

impl HeldStart {
    fn is_first(&self, operation: &OperationId) -> bool {
        self.first.lock_unpoisoned().as_ref() == Some(operation)
    }
}

fn finished() -> Result<CommandEvent, EnvError> {
    Ok(CommandEvent::Finished {
        outcome: Outcome::Completed,
        exit_code: Some(0),
        timed_out: false,
        cwd_reset: None,
        output_bytes: 0,
        output_limit: None,
    })
}

#[async_trait]
impl Commands for HeldStart {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        _request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        let operation = context.operation.clone();
        {
            let mut first = self.first.lock_unpoisoned();
            if first.is_none() {
                *first = Some(context.operation);
                self.started.notify_one();
            }
        }
        Ok(self.watch_command(operation, 0))
    }

    fn watch_command(
        &self,
        operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        let started = Ok(CommandEvent::Started {
            output: EnvPath::new("/tmp/hook-out"),
        });
        if !self.is_first(&operation) {
            return stream::iter([started, finished()]).boxed();
        }
        let mut release = self.release.subscribe();
        let end = stream::once(async move {
            let _ = release.wait_for(|go| *go).await;
            finished()
        });
        stream::iter([started]).chain(end).boxed()
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Ok(())
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

/// 一个 SessionStart command hook（命令本身由 `HeldStart` 扮演）。
fn start_hook() -> HooksConfig {
    let settings = serde_json::json!({"hooks": {
        "SessionStart": [{"hooks": [{"type": "command", "command": "slow-start"}]}],
    }});
    let (parsed, warnings) = HooksSettings::from_json(&settings);
    assert!(warnings.is_empty(), "{warnings:?}");
    HooksConfig {
        version: 1,
        sources: vec![(HookSource::User, parsed)],
        ..HooksConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_message_waits_for_the_session_start_hook_in_the_conversation() {
    let env = Arc::new(HeldStart {
        first: Mutex::new(None),
        started: Notify::new(),
        release: watch::Sender::new(false),
    });
    let model = GatedModel::scripted(vec![text_response(&["hello back"])]);
    let setup = Setup {
        environments: registry(unsupported_machine().with_commands(env.clone())),
        hooks: Some(start_hook()),
        ..Setup::default()
    };
    let held = env.clone();
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first message");
        held.started.notified().await;
        ui.wait_for("first message").await;
        // hook 还扣着：第一条输入在内核的队列里。给事件一点时间折进来，画面上仍然没有“下一轮”的栈。
        tokio::time::sleep(Duration::from_millis(300)).await;
        let screen = ui.screen();
        assert!(screen.contains("first message"), "{screen}");
        assert!(!screen.contains("next turn"), "{screen}");
        held.release.send_replace(true);
        let screen = ui.wait_for("hello back").await;
        assert!(!screen.contains("next turn"), "{screen}");
    })
    .await;
}
