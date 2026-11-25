//! 会话钉住的 system 输入（docs/specs/rust-cli/context.md §15.3、§15.4）：LocalKernel + 真实主循环 + 上下文 + 压缩，看模型请求的
//! system。建会话时钉内核的 `--append-system-prompt`；另一个内核（另一次启动，参数不同）冷打开同一个会话时沿用记下的
//! （context.md §15.4）；压缩的摘要请求仍用钉住的那一份，压缩之后按这个内核的输入重钉。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod session_pin {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
    use rust_cli_kernel_api::model::{ModelRequest, QuerySource};
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, EndReason, Kernel, KernelEvent,
        Persistence, SessionHandle, TurnOutcome,
    };
    use rust_cli_kernel_api::session_prompt::{FrontendTraits, SessionPrompt};
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
        selection, text_response,
    };
    use rust_cli_runtime::{ContextConfig, Defaults, LocalKernel, Ports, Surface};

    const FIRST: &str = "Always end your reply with the word DONE.";
    const LATER: &str = "Always end your reply with the word OVER.";

    fn kernel(model: Arc<ScriptedModel>, store: Arc<MemoryStore>, append: &str) -> LocalKernel {
        LocalKernel::with_context(
            Ports {
                model,
                environments: environments(Arc::new(UnusedEnvironment)),
                store: store.clone(),
                artifacts: store,
                clock: Arc::new(FixedClock(1_000)),
                ids: Arc::new(SequentialIds::default()),
            },
            Defaults {
                selection: selection().into(),
                mode: PermissionMode::DontAsk,
                max_turns: None,
                search_tools: false,
                task_tools: None,
                limits: Default::default(),
                additional_directories: Vec::new(),
            },
            ContextConfig::new(Surface::Print),
        )
        .with_prompt(SessionPrompt::new(
            None,
            Some(append.to_owned()),
            FrontendTraits::default(),
        ))
    }

    fn create() -> CreateSession {
        CreateSession {
            id: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/work"),
            },
            selection: selection(),
            mode: PermissionMode::DontAsk,
            plan: false,
            persistence: Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        }
    }

    fn command(id: &str, command: Command) -> CommandEnvelope {
        CommandEnvelope {
            id: CommandId::new(id),
            command,
        }
    }

    fn submit(text: &str) -> Command {
        Command::Submit {
            text: text.to_owned(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        }
    }

    async fn finish(session: &Arc<dyn SessionHandle>, after: u64) -> u64 {
        let mut events = session.subscribe(Some(after));
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            if let KernelEvent::TurnFinished { outcome, .. } = event.event {
                assert_eq!(outcome, TurnOutcome::Success);
                return event.seq;
            }
        }
    }

    /// 会话段（system 的最后一块）的最后一段。
    fn appended(request: &ModelRequest) -> &str {
        let session = &request.system.last().unwrap().text;
        session.rsplit("\n\n").next().unwrap()
    }

    #[tokio::test]
    async fn a_resumed_session_keeps_its_system_until_it_is_compacted() {
        let store = Arc::new(MemoryStore::default());
        let first_model = Arc::new(ScriptedModel::new(vec![text_response(&["Hello. DONE"])]));
        let first = kernel(first_model.clone(), store.clone(), FIRST);
        let id = first.create_session(create()).await.unwrap();
        let session = first.open_session(&id).await.unwrap();
        session.send(command("c1", submit("Say hello."))).await;
        finish(&session, 0).await;
        assert_eq!(appended(&first_model.requests()[0]), FIRST);
        session.close(EndReason::Clear).await;

        // 另一次启动换了参数：续写的请求仍是第一次记下的
        let model = Arc::new(ScriptedModel::new(vec![
            text_response(&["Hello again. DONE"]),
            text_response(&["<summary>Earlier work.</summary>"]),
            text_response(&["After the summary. OVER"]),
        ]));
        let second = kernel(model.clone(), store, LATER);
        let session = second.open_session(&id).await.unwrap();
        session.send(command("c2", submit("Say it again."))).await;
        let after = finish(&session, 0).await;
        session
            .send(command("c3", Command::Compact { instructions: None }))
            .await;
        let after = finish(&session, after).await;
        session.send(command("c4", submit("Go on."))).await;
        finish(&session, after).await;
        let requests = model.requests();
        assert_eq!(appended(&requests[0]), FIRST);
        // 摘要请求与压缩前的主线程逐字相同（压缩完才重钉）
        assert_eq!(requests[1].source, QuerySource::Compact);
        assert_eq!(appended(&requests[1]), FIRST);
        // 压缩之后按这一次启动的输入重钉
        assert_eq!(appended(&requests[2]), LATER);
    }
}
