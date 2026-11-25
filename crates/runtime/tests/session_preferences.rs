//! 宿主给的会话偏好（docs/specs/rust-cli/app-prefs.md §4.2、§4.3）：LocalKernel + 真实主循环 + 上下文 + agents + 压缩，看会话的视图与
//! 模型请求。偏好进会话的起始状态（建会话、冷打开）；关了 ToolSearch 时主线程与子代理的工具表里没有它，关了自动记忆时主线程与
//! 摘要请求的 system 里没有记忆段。缺省的偏好什么都不关（对照）。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod session_preferences {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
    use rust_cli_kernel_api::model::{ModelRequest, QuerySource};
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, EndReason, Kernel, KernelEvent,
        Persistence, SessionHandle, StartSource, TurnOutcome,
    };
    use rust_cli_kernel_api::session_preferences::{OpenStart, SessionPreferences};
    use rust_cli_kernel_api::store::SessionStore;
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
        selection, text_response, tool_use_response,
    };
    use rust_cli_runtime::{ContextConfig, Defaults, LocalKernel, Ports, Surface};
    use serde_json::json;

    /// 内核的记忆目录（组装时给）：system 的记忆段写它。
    const MEMORY_DIR: &str = "/Users/dev/.zcode/projects/-Users-dev-work/memory/";
    const OFF: SessionPreferences = SessionPreferences {
        auto_memory: false,
        tool_search: false,
    };

    fn kernel(model: Arc<ScriptedModel>, store: Arc<MemoryStore>) -> LocalKernel {
        let mut context = ContextConfig::new(Surface::Interactive);
        context.memory_dir = Some(MEMORY_DIR.to_owned());
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
            context,
        )
    }

    fn create(preferences: SessionPreferences) -> CreateSession {
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
            preferences,
        }
    }

    fn has_tool(request: &ModelRequest, name: &str) -> bool {
        request.tools.iter().any(|tool| tool.name == name)
    }

    fn has_memory_section(request: &ModelRequest) -> bool {
        request
            .system
            .iter()
            .any(|block| block.text.contains(MEMORY_DIR))
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

    /// 主线程派一个前台子代理再回答：返回主线程第一个请求与子代理的第一个请求。
    async fn one_turn_with_a_subagent(
        preferences: SessionPreferences,
    ) -> (ModelRequest, ModelRequest) {
        let subagent = Arc::new(ScriptedModel::new(vec![text_response(&["Child done."])]));
        let model = Arc::new(
            ScriptedModel::new(vec![
                tool_use_response(
                    "toolu_1",
                    "Agent",
                    &json!({"description": "Say hello", "prompt": "Say hello.", "subagent_type": "general-purpose", "run_in_background": false}),
                ),
                text_response(&["Main done."]),
            ])
            .by_source(
                |source| matches!(source, QuerySource::Subagent(_)),
                subagent.clone(),
            ),
        );
        let kernel = kernel(model.clone(), Arc::new(MemoryStore::default()));
        let id = kernel.create_session(create(preferences)).await.unwrap();
        let session = kernel.open_session(&id).await.unwrap();
        assert_eq!(session.view().await.unwrap().preferences, preferences);
        session.send(command("c1", submit("Run the checks."))).await;
        finish(&session, 0).await;
        (model.requests()[0].clone(), subagent.requests()[0].clone())
    }

    #[tokio::test]
    async fn the_default_preferences_keep_tool_search_and_memory() {
        let (main, child) = one_turn_with_a_subagent(SessionPreferences::default()).await;
        assert!(has_tool(&main, "ToolSearch"));
        assert!(has_tool(&child, "ToolSearch"));
        assert!(has_memory_section(&main));
    }

    /// App 关了两项：主线程的请求没有 ToolSearch、没有记忆段；子代理从父运行继承，同样没有 ToolSearch。
    #[tokio::test]
    async fn preferences_turned_off_reach_the_main_thread_and_the_subagent() {
        let (main, child) = one_turn_with_a_subagent(OFF).await;
        assert!(!has_tool(&main, "ToolSearch"), "{:?}", main.tools);
        assert!(!has_memory_section(&main));
        assert!(!has_tool(&child, "ToolSearch"), "{:?}", child.tools);
        // 别的工具照常
        assert!(has_tool(&main, "Agent") && has_tool(&main, "Read"));
    }

    /// 压缩的摘要请求带着会话的偏好：关了自动记忆时它的 system 里也没有记忆段。
    #[tokio::test]
    async fn the_compaction_summary_request_follows_the_session_preferences() {
        let model = Arc::new(ScriptedModel::new(vec![
            text_response(&["First answer."]),
            text_response(&["<summary>Earlier work.</summary>"]),
        ]));
        let kernel = kernel(model.clone(), Arc::new(MemoryStore::default()));
        let id = kernel.create_session(create(OFF)).await.unwrap();
        let session = kernel.open_session(&id).await.unwrap();
        session.send(command("c1", submit("Hello."))).await;
        let after = finish(&session, 0).await;
        session
            .send(command("c2", Command::Compact { instructions: None }))
            .await;
        finish(&session, after).await;
        let summary = model
            .requests()
            .into_iter()
            .find(|request| request.source == QuerySource::Compact)
            .unwrap();
        assert!(!has_memory_section(&summary));
        assert!(!has_tool(&summary, "ToolSearch"));
    }

    /// 冷打开时调用方给的偏好进起始状态；已经打开着的会话不跟（运行实例存在时不刷新）。
    #[tokio::test]
    async fn a_cold_open_takes_the_given_preferences_and_an_open_session_keeps_its_own() {
        let store = Arc::new(MemoryStore::default());
        let first = kernel(Arc::new(ScriptedModel::new(Vec::new())), store.clone());
        let id = first
            .create_session(create(SessionPreferences::default()))
            .await
            .unwrap();
        first
            .open_session(&id)
            .await
            .unwrap()
            .close(EndReason::Clear)
            .await;
        assert!(store.load(&id).await.is_ok());
        let second = kernel(Arc::new(ScriptedModel::new(Vec::new())), store);
        let start = || OpenStart {
            directories: Vec::new(),
            preferences: OFF,
        };
        let open = second
            .open_session_with(&id, StartSource::Resume, start())
            .await
            .unwrap();
        assert_eq!(open.view().await.unwrap().preferences, OFF);
        let again = second
            .open_session_with(
                &id,
                StartSource::Resume,
                OpenStart {
                    preferences: SessionPreferences::default(),
                    ..start()
                },
            )
            .await
            .unwrap();
        assert_eq!(again.view().await.unwrap().preferences, OFF);
    }
}
