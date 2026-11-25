//! 丢掉 `LocalKernel` 之后，组装时建的对象图要全部释放（docs/issues/rust-cli-kernel-release/01-dropped-kernel-not-freed.md）。
//!
//! 常驻进程按需组装、丢掉内核（一个进程服务多个用户的会话）：留下的部分会连同注入的端口一直占着内存。这里用端口的
//! 强引用数判断：丢掉内核后，测试自己之外不应再有人持有会话库、模型与执行环境注册表。

// 集成测试里的辅助函数也按测试代码处理（clippy 的 allow-unwrap-in-tests 只认 cfg(test)）。
#[cfg(test)]
mod kernel_release {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
    use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::session_api::{
        Command, CommandEnvelope, CreateSession, Delivery, EndReason, Kernel, KernelEvent,
    };
    use rust_cli_kernel_api::testing::{
        FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, environments,
        selection, text_response,
    };
    use rust_cli_runtime::{Defaults, LocalKernel, Ports};

    /// 测试自己持有的端口；`counts` 是它们各自的强引用数。
    struct Held {
        store: Arc<MemoryStore>,
        model: Arc<ScriptedModel>,
        environments: Arc<dyn EnvironmentRegistry>,
    }

    impl Held {
        fn new() -> Self {
            let env = Arc::new(MemoryEnvironment::new(EnvFacts {
                cwd: EnvPath::new("/workspace"),
                platform: "linux".into(),
                os_version: "test".into(),
                shell: "sh".into(),
                is_git_repo: false,
                home: None,
                uid: None,
            }));
            Self {
                store: Arc::new(MemoryStore::default()),
                model: Arc::new(ScriptedModel::new(vec![text_response(&["done"])])),
                environments: environments(env),
            }
        }

        fn counts(&self) -> (usize, usize, usize) {
            (
                Arc::strong_count(&self.store),
                Arc::strong_count(&self.model),
                Arc::strong_count(&self.environments),
            )
        }

        fn kernel(&self) -> LocalKernel {
            LocalKernel::new(
                Ports {
                    model: self.model.clone(),
                    environments: self.environments.clone(),
                    store: self.store.clone(),
                    artifacts: self.store.clone(),
                    clock: Arc::new(FixedClock(1_000)),
                    ids: Arc::new(SequentialIds::default()),
                },
                Defaults {
                    selection: selection().into(),
                    mode: PermissionMode::DontAsk,
                    max_turns: None,
                    search_tools: false,
                    task_tools: None,
                    limits: rust_cli_runtime::Limits::default(),
                    additional_directories: Vec::new(),
                },
            )
        }
    }

    fn workspace() -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/workspace"),
        }
    }

    /// 丢掉内核时，作用域里的任务被中止；被中止的任务要等运行时再轮到它们才真正释放。
    async fn settle() {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_dropped_kernel_releases_its_ports() {
        let held = Held::new();
        let before = held.counts();
        let kernel = held.kernel();
        assert_ne!(
            held.counts(),
            before,
            "the kernel holds the ports while it lives"
        );
        drop(kernel);
        settle().await;
        assert_eq!(held.counts(), before);
    }

    #[tokio::test]
    async fn a_dropped_kernel_releases_its_ports_after_a_closed_session() {
        let held = Held::new();
        let before = held.counts();
        let kernel = held.kernel();
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: workspace(),
                selection: selection(),
                mode: PermissionMode::DontAsk,
                plan: false,
                persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
                additional_directories: Vec::new(),
                preferences: Default::default(),
            })
            .await
            .unwrap();
        let session = kernel.open_session(&id).await.unwrap();
        let mut events = session.subscribe(None);
        session
            .send(CommandEnvelope {
                id: CommandId::new("c1"),
                command: Command::Submit {
                    text: "hello".to_owned(),
                    extras: Default::default(),
                    delivery: Delivery::Queue,
                    target: None,
                },
            })
            .await;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                break;
            }
        }
        drop(events);
        session.close(EndReason::Other).await;
        drop(session);
        drop(kernel);
        settle().await;
        assert_eq!(held.counts(), before);
    }
}
