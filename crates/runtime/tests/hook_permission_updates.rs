//! PermissionRequest hook 的权限更新经 LocalKernel 的真实组装（hooks 引擎、执行管线、判决链、会话 actor）落下
//! （docs/specs/rust-cli/permission-updates.md §5）：hook 先答时随回答落，同一个会话里下一次调用按新的会话层与模式判决。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod hook_permission_updates {
    use rust_cli_base::sync::LockUnpoisoned;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures_util::StreamExt;
    use futures_util::stream::BoxStream;
    use rust_cli_kernel_api::env::{
        CommandEvent, CommandKind, EnvError, EnvPath, OperationContext, Outcome, OutputStream,
        StartCommand, WorkspaceRef,
    };
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, OperationId, SessionId};
    use rust_cli_kernel_api::machine::Commands;
    use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent};
    use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior, RuleSource};
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, SequencedEvent,
        SessionHandle,
    };
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryStore, ScriptedModel, SequentialIds, registry, selection, text_response,
        tool_use_response, unsupported_machine,
    };
    use rust_cli_runtime::{
        ContextConfig, Defaults, HookSource, HooksConfig, HooksSettings, LocalKernel, Ports,
        Surface,
    };
    use serde_json::{Value, json};
    use tokio::sync::Notify;

    /// 命令端口：hook 命令按顺序回 `replies` 里的 JSON（`gate` 有值时先等它放行）；别的命令（Bash 工具）只记下，立即结束。
    #[derive(Default)]
    struct HookEnv {
        replies: Mutex<VecDeque<Value>>,
        gate: Option<Arc<Notify>>,
        hooks: Mutex<usize>,
        commands: Mutex<Vec<String>>,
    }

    fn finished() -> CommandEvent {
        CommandEvent::Finished {
            outcome: Outcome::Completed,
            exit_code: Some(0),
            timed_out: false,
            cwd_reset: None,
            output_bytes: 0,
            output_limit: None,
        }
    }

    #[async_trait]
    impl Commands for HookEnv {
        async fn start_command(
            &self,
            _workspace: &WorkspaceRef,
            _context: OperationContext,
            request: StartCommand,
        ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
            let started = CommandEvent::Started {
                output: EnvPath::new("/tmp/out"),
            };
            if !matches!(request.kind, CommandKind::Hook { .. }) {
                self.commands.lock_unpoisoned().push(request.command);
                let events = [started, finished()];
                return Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed());
            }
            *self.hooks.lock_unpoisoned() += 1;
            let reply = self
                .replies
                .lock_unpoisoned()
                .pop_front()
                .unwrap_or_else(|| json!({}));
            let gate = self.gate.clone();
            let output = async move {
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                CommandEvent::Output {
                    stream: OutputStream::Stdout,
                    data: reply.to_string().into_bytes(),
                    cursor: 0,
                }
            };
            let rest = futures_util::stream::once(output)
                .chain(futures_util::stream::iter([finished()]))
                .map(Ok);
            Ok(futures_util::stream::iter([Ok(started)])
                .chain(rest)
                .boxed())
        }

        fn watch_command(
            &self,
            _operation: OperationId,
            _cursor: u64,
        ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
            futures_util::stream::iter([Err(EnvError::Unsupported {
                what: "test environment".to_owned(),
            })])
            .boxed()
        }

        async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
            Ok(())
        }

        async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
            Ok(())
        }
    }

    fn workspace() -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        }
    }

    /// hook 的放行，带 `updatedPermissions`。
    fn allow_with(updates: Value) -> Value {
        json!({"hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow", "updatedPermissions": updates},
        }})
    }

    fn permission_hook() -> HooksConfig {
        let (settings, warnings) = HooksSettings::from_json(&json!({"hooks": {
            "PermissionRequest": [{
                "matcher": "Bash",
                "hooks": [{"type": "command", "command": "grant"}],
            }],
        }}));
        assert!(warnings.is_empty(), "{warnings:?}");
        HooksConfig {
            sources: vec![(HookSource::User, settings)],
            ..HooksConfig::default()
        }
    }

    struct Run {
        model: Arc<ScriptedModel>,
        env: Arc<HookEnv>,
        store: Arc<MemoryStore>,
        session: Arc<dyn SessionHandle>,
        events: BoxStream<'static, SequencedEvent>,
        // 内核要活到测试结束。
        _kernel: LocalKernel,
    }

    /// default 模式的会话：模型先后调用两条 Bash 命令（`first`、`second`），然后收尾。
    async fn start(surface: Surface, env: HookEnv, first: &str, second: &str) -> Run {
        let model = Arc::new(ScriptedModel::new(vec![
            tool_use_response("toolu_1", "Bash", &json!({"command": first})),
            tool_use_response("toolu_2", "Bash", &json!({"command": second})),
            text_response(&["done"]),
        ]));
        let env = Arc::new(env);
        let store = Arc::new(MemoryStore::default());
        let kernel = LocalKernel::with_context(
            Ports {
                model: model.clone(),
                environments: registry(unsupported_machine().with_commands(env.clone())),
                store: store.clone(),
                artifacts: store.clone(),
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
            ContextConfig::new(surface),
        );
        kernel.replace_hooks(permission_hook());
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: workspace(),
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
        let events = session.subscribe(Some(0));
        session
            .send(CommandEnvelope {
                id: CommandId::new("c1"),
                command: Command::Submit {
                    text: "run them".to_owned(),
                    extras: Default::default(),
                    delivery: Delivery::Queue,
                    target: None,
                },
            })
            .await;
        Run {
            model,
            env,
            store,
            session,
            events,
            _kernel: kernel,
        }
    }

    async fn next(events: &mut BoxStream<'static, SequencedEvent>) -> KernelEvent {
        tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .expect("the turn must go on")
            .unwrap()
            .event
    }

    async fn finish(run: &mut Run) {
        while !matches!(
            next(&mut run.events).await,
            KernelEvent::TurnFinished { .. }
        ) {}
    }

    /// 最后一个请求里 `tool_use` 的结果：（文字，是不是错误）。
    fn result(model: &ScriptedModel, tool_use: &str) -> (String, bool) {
        let requests = model.requests();
        requests
            .last()
            .unwrap()
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|content| match &content.block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } if tool_use_id.as_str() == tool_use => Some((
                    content
                        .iter()
                        .filter_map(|part| match part {
                            ToolResultContent::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                    *is_error,
                )),
                _ => None,
            })
            .expect("the last request carries the result")
    }

    /// 交互界面里 hook 先答：会话层的 deny 规则与模式随回答落下，第二条命令按 deny 规则拒绝（不再询问、不再跑 hook），会话切到
    /// acceptEdits。修前（HOOKS-2）只认允许规则加持久的 destination，第二条命令照常询问、hook 再跑一次。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hook_that_answers_first_brings_session_rules_and_the_mode() {
        let env = HookEnv {
            replies: Mutex::new(VecDeque::from([allow_with(json!([
                {"type": "addRules", "behavior": "deny", "destination": "session",
                 "rules": [{"toolName": "Bash", "ruleContent": "rm:*"}]},
                {"type": "setMode", "mode": "acceptEdits", "destination": "session"},
            ]))])),
            ..HookEnv::default()
        };
        let mut run = start(Surface::Interactive, env, "touch a", "rm -f a").await;
        finish(&mut run).await;
        assert_eq!(*run.env.hooks.lock_unpoisoned(), 1);
        let commands = run.env.commands.lock_unpoisoned().clone();
        assert!(
            commands.iter().all(|command| !command.contains("rm -f a")),
            "{commands:?}"
        );
        let (text, is_error) = result(&run.model, "toolu_2");
        assert!(is_error, "{text}");
        assert!(text.contains("denied"), "{text}");
        let view = run.session.view().await.unwrap();
        assert_eq!(view.mode, PermissionMode::AcceptEdits);
        assert_eq!(
            view.session_rules
                .iter()
                .map(|rule| (rule.source, rule.behavior))
                .collect::<Vec<_>>(),
            [(RuleSource::Session, RuleBehavior::Deny)]
        );
        // 只在会话里：本机项目规则集里没有它。
        assert!(
            rust_cli_kernel_api::store::LocalSettings::project_rules(&*run.store, &workspace())
                .await
                .unwrap()
                .deny
                .is_empty()
        );
    }

    /// `-p` 里没有人能回答（hooks.md §13 第 3 项）：headless 的 `Unavailable` 先到 owner、hook 后给结论时，管线另请 owner 落 hook 的规则
    /// （本机项目规则集），第二条命令按它放行、不再询问。修前（HOOKS-2）这时规则不写，第二条命令又问一次（hook 先到时才写，结局看先后）。
    #[tokio::test(flavor = "multi_thread")]
    async fn print_mode_keeps_the_hook_rules_when_nobody_answers_first() {
        let gate = Arc::new(Notify::new());
        let env = HookEnv {
            replies: Mutex::new(VecDeque::from([allow_with(json!([
                {"type": "addRules", "behavior": "allow", "destination": "localSettings",
                 "rules": [{"toolName": "Bash", "ruleContent": "touch second"}]},
            ]))])),
            gate: Some(gate.clone()),
            ..HookEnv::default()
        };
        let mut run = start(Surface::Print, env, "touch first", "touch second").await;
        let mut asked = 0;
        loop {
            match next(&mut run.events).await {
                KernelEvent::InteractionAsked { interaction, .. } => {
                    asked += 1;
                    assert_eq!(asked, 1, "the kept rule must let the second command run");
                    // 同 headless：当场答“没有人能回答”，然后才放 hook 的结论出来。
                    let answer = rust_cli_kernel_api::orchestration::PermissionAnswer::Unavailable;
                    run.session
                        .send(CommandEnvelope {
                            id: CommandId::new("c2"),
                            command: Command::ResolveInteraction {
                                interaction,
                                answer: answer.into(),
                            },
                        })
                        .await;
                    gate.notify_one();
                }
                KernelEvent::TurnFinished { .. } => break,
                _ => {}
            }
        }
        let commands = run.env.commands.lock_unpoisoned().clone();
        assert!(
            commands
                .iter()
                .any(|command| command.contains("touch second")),
            "{commands:?}"
        );
        let (text, is_error) = result(&run.model, "toolu_2");
        assert!(!is_error, "{text}");
        let rules =
            rust_cli_kernel_api::store::LocalSettings::project_rules(&*run.store, &workspace())
                .await
                .unwrap();
        assert_eq!(
            rules.allow,
            [rust_cli_kernel_api::permission::syntax::parse_rule(
                "Bash(touch second)"
            )]
        );
    }
}
