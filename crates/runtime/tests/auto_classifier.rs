//! 判决链里的分类器（docs/specs/rust-cli/auto-classifier.md §16.9）：经 LocalKernel 的真实组装，plan 里不是只读的 Bash 交分类器——
//! 发出 `auto_mode` 来源的旁路请求；放行时命令照常执行，拦下时不执行、模型看到拦下的说明。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod auto_classifier {
    use rust_cli_base::sync::LockUnpoisoned;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures_util::StreamExt;
    use futures_util::stream::BoxStream;
    use rust_cli_kernel_api::env::{
        CommandEvent, EnvError, EnvPath, OperationContext, Outcome, StartCommand, WorkspaceRef,
    };
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, OperationId, SessionId};
    use rust_cli_kernel_api::machine::Commands;
    use rust_cli_kernel_api::model::QuerySource;
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
    };
    use rust_cli_kernel_api::side_request::SideSource;
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, registry, selection, text_response,
        tool_use_response, unsupported_machine,
    };
    use rust_cli_runtime::{ContextConfig, Defaults, LocalKernel, Ports, Surface};
    use serde_json::json;

    /// 命令端口：只记下跑过的命令，立即结束。
    #[derive(Default)]
    struct Recorded(Mutex<Vec<String>>);

    #[async_trait]
    impl Commands for Recorded {
        async fn start_command(
            &self,
            _workspace: &WorkspaceRef,
            _context: OperationContext,
            request: StartCommand,
        ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
            self.0.lock_unpoisoned().push(request.command);
            let events = [
                CommandEvent::Started {
                    output: EnvPath::new("/tmp/out"),
                },
                CommandEvent::Finished {
                    outcome: Outcome::Completed,
                    exit_code: Some(0),
                    timed_out: false,
                    cwd_reset: None,
                    output_bytes: 0,
                    output_limit: None,
                },
            ];
            Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed())
        }

        fn watch_command(
            &self,
            _operation: OperationId,
            _cursor: u64,
        ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
            futures_util::stream::empty().boxed()
        }

        async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
            Ok(())
        }

        async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
            Ok(())
        }
    }

    /// plan 模式的会话里模型跑一次 `touch plan.txt`，分类器的各阶段依次回 `stages`（主模型不在分数表里，是 `<block>` 格式）。返回
    /// 模型收到的请求与跑过的命令。
    async fn plan_touch(stages: &[&str]) -> (Arc<ScriptedModel>, Vec<String>) {
        let mut script = vec![tool_use_response(
            "toolu_touch",
            "Bash",
            &json!({"command": "touch plan.txt"}),
        )];
        script.extend(stages.iter().map(|stage| text_response(&[stage])));
        script.push(text_response(&["done"]));
        let model = Arc::new(ScriptedModel::new(script));
        let commands = Arc::new(Recorded::default());
        let store = Arc::new(MemoryStore::default());
        let kernel = LocalKernel::with_context(
            Ports {
                model: model.clone(),
                environments: registry(unsupported_machine().with_commands(commands.clone())),
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
            ContextConfig::new(Surface::Print),
        );
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new("/work"),
                },
                selection: selection(),
                mode: PermissionMode::Default,
                plan: true,
                persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
                additional_directories: Vec::new(),
                preferences: Default::default(),
            })
            .await
            .unwrap();
        let session = kernel.open_session(&id).await.unwrap();
        let mut events = session.subscribe(Some(0));
        session
            .send(CommandEnvelope {
                id: CommandId::new("c1"),
                command: Command::Submit {
                    text: "make a plan file".to_owned(),
                    extras: Default::default(),
                    delivery: Delivery::Queue,
                    target: None,
                },
            })
            .await;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), events.next())
                .await
                .expect("the turn must finish")
                .unwrap();
            assert!(
                !matches!(event.event, KernelEvent::InteractionAsked { .. }),
                "the classifier decides, nobody is asked: {event:?}"
            );
            if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                break;
            }
        }
        let run = commands.0.lock_unpoisoned().clone();
        (model, run)
    }

    #[tokio::test]
    async fn plan_mode_asks_the_classifier_and_follows_its_verdict() {
        let (model, run) = plan_touch(&["<block>no</block>"]).await;
        let requests = model.requests();
        assert_eq!(
            requests[1].source,
            QuerySource::Side(SideSource::AutoMode),
            "the second request is the classifier's"
        );
        assert_eq!(run, ["touch plan.txt"]);
        let blocked = "<block>yes</block><reason>[Git Destructive] no</reason>";
        let (model, run) = plan_touch(&[blocked, blocked]).await;
        assert!(run.is_empty(), "{run:?}");
        let last = format!(
            "{:?}",
            model.requests().last().unwrap().messages.last().unwrap()
        );
        assert!(
            last.contains(
                "denied by the ZCode auto mode classifier. Reason: [Git Destructive] no."
            ),
            "{last}"
        );
    }
}
