use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelRequest};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, EndReason, Kernel, KernelEvent,
};
use rust_cli_kernel_api::store::LocalSettings;
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryArtifacts, MemoryEnvironment, ScriptedModel, SequentialIds, environments,
    selection, text_response,
};
use rust_cli_runtime::{Limits, Surface};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::*;

struct WaitingModel {
    entered: Notify,
    release: Arc<Notify>,
    requests: Mutex<Vec<ModelRequest>>,
}
impl ModelPort for WaitingModel {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.requests.lock().unwrap().push(request);
        self.entered.notify_one();
        let release = self.release.clone();
        stream::once(async move {
            tokio::select! { _ = release.notified() => text_response(&["first answer"]), _ = cancel.cancelled() => Vec::new() }
        }).flat_map(stream::iter).boxed()
    }
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/workspace"),
    }
}

fn host(
    database: &PgDatabase,
    tenant: &str,
    model: Arc<dyn ModelPort>,
    ids: Arc<dyn Ids>,
) -> LocalKernel {
    let env = Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/workspace"),
        platform: "linux".into(),
        os_version: "test".into(),
        shell: "sh".into(),
        is_git_repo: false,
        home: None,
        uid: None,
    }));
    let access = StoreAccessContext::verified(tenant, u64::MAX).unwrap();
    kernel(
        database,
        CloudSessionConfig {
            access,
            projects: vec![StoreProject {
                id: "project".into(),
                workspace: workspace(),
            }],
            defaults: Defaults {
                selection: selection().into(),
                mode: PermissionMode::Default,
                max_turns: Some(4),
                search_tools: false,
                task_tools: None,
                limits: Limits::default(),
                additional_directories: Vec::new(),
            },
            context: ContextConfig::new(Surface::Print),
        },
        CloudPorts {
            model,
            environments: environments(env),
            artifacts: Arc::new(MemoryArtifacts::default()),
            clock: Arc::new(FixedClock(1_000)),
            ids,
            uuids: Arc::new(rust_cli_node_store::NodeIds::new(Arc::new(FixedClock(
                1_000,
            )))),
        },
    )
    .unwrap()
}

fn submit(id: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Submit {
            text: text.into(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        },
    }
}

#[tokio::test]
#[ignore = "requires the isolated PG integration runner"]
#[expect(
    clippy::disallowed_methods,
    reason = "云端 Harness 测试读取独立 PG 连接"
)]
async fn accepted_queue_survives_worker_loss_and_waits_for_user() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let url = RedactedString::new(
        std::env::var(rust_cli_base::names::env::ZCODE_TEST_POSTGRES_URL).unwrap(),
    );
    let database = PgDatabase::connect_loopback_test(&url, 4).await.unwrap();
    let tenant = format!(
        "harness-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    );
    let ids: Arc<dyn Ids> = Arc::new(SequentialIds::default());
    let waiting = Arc::new(WaitingModel {
        entered: Notify::new(),
        release: Arc::new(Notify::new()),
        requests: Mutex::new(Vec::new()),
    });
    let first = host(&database, &tenant, waiting.clone(), ids.clone());
    let session = first
        .create_session(CreateSession {
            id: Some(SessionId::new("conversation")),
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
    let handle = first.open_session(&session).await.unwrap();
    assert!(matches!(
        handle.send(submit("one", "first input")).await,
        Ack::Accepted { .. }
    ));
    tokio::time::timeout(Duration::from_secs(10), waiting.entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        handle.send(submit("two", "queued input")).await,
        Ack::Accepted { .. }
    ));
    assert_eq!(handle.view().await.unwrap().queue.len(), 1);
    drop(handle);
    drop(first);

    let model = Arc::new(ScriptedModel::new(vec![text_response(&["recovered"])]));
    let second = host(&database, &tenant, model.clone(), ids);
    // 旧宿主已经终止；同一新 Kernel 的并发打开必须立即复用同一个 Actor。
    let (restored, same) =
        tokio::try_join!(second.open_session(&session), second.open_session(&session)).unwrap();
    assert!(Arc::ptr_eq(&restored, &same));
    assert_eq!(restored.view().await.unwrap().queue.len(), 1);
    assert!(
        model.requests().is_empty(),
        "opening must not run queued inputs"
    );
    assert!(matches!(
        second
            .input_receipt(&session, &CommandId::new("two"))
            .await
            .unwrap(),
        Some(Ack::Accepted { .. })
    ));
    let mut events = restored.subscribe(None);
    assert!(matches!(
        restored.send(submit("three", "continue")).await,
        Ack::Accepted { .. }
    ));
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = events.next().await {
            if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                return;
            }
        }
        panic!("session closed without finishing");
    })
    .await
    .unwrap();
    let view = restored.view().await.unwrap();
    assert!(view.queue.is_empty());
    for expected in ["queued input", "continue"] {
        let count = view.messages.iter().filter(|message| matches!(message, Message::User(user) if user.content.iter().any(|block| matches!(block, ContentBlock::Text { text } if text == expected)))).count();
        assert_eq!(count, 1, "durable input must enter history once");
    }
    restored.close(EndReason::Other).await;
    drop(restored);
    drop(second);
    database.close().await;
}

struct ParentAndChild {
    parent: ScriptedModel,
    child: ScriptedModel,
}
impl ModelPort for ParentAndChild {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        match request.source {
            rust_cli_kernel_api::model::QuerySource::Subagent(_) => {
                self.child.stream(request, cancel)
            }
            _ => self.parent.stream(request, cancel),
        }
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
#[expect(clippy::disallowed_methods, reason = "云端权限测试读取独立 PG 连接")]
async fn project_rule_changes_reach_child_execution() {
    use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};
    use rust_cli_kernel_api::store::ProjectRulesChange;
    use rust_cli_kernel_api::testing::tool_use_response;
    let database = PgDatabase::connect_loopback_test(
        &RedactedString::new(
            std::env::var(rust_cli_base::names::env::ZCODE_TEST_POSTGRES_URL).unwrap(),
        ),
        4,
    )
    .await
    .unwrap();
    let tenant = format!("child-rules-{}", std::process::id());
    let ids: Arc<dyn Ids> = Arc::new(SequentialIds::default());
    let model = Arc::new(ParentAndChild {
        parent: ScriptedModel::new(vec![
            tool_use_response(
                "agent",
                rust_cli_kernel_api::tool_names::AGENT,
                &serde_json::json!({"description":"check", "prompt":"check", "subagent_type":"general-purpose", "run_in_background":false}),
            ),
            text_response(&["done"]),
        ]),
        child: ScriptedModel::new(vec![
            tool_use_response(
                "write",
                rust_cli_kernel_api::tool_names::WRITE,
                &serde_json::json!({"file_path":"/workspace/denied", "content":"no"}),
            ),
            text_response(&["denied"]),
        ]),
    });
    let kernel = host(&database, &tenant, model.clone(), ids.clone());
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::BypassPermissions,
            plan: false,
            persistence: rust_cli_kernel_api::session_api::Persistence::Immediate,
            additional_directories: Vec::new(),
            preferences: Default::default(),
        })
        .await
        .unwrap();
    let access = StoreAccessContext::verified(&tenant, u64::MAX).unwrap();
    let settings = database
        .bind(
            access,
            vec![StoreProject {
                id: "project".into(),
                workspace: workspace(),
            }],
            Arc::new(FixedClock(1_000)),
            Arc::new(rust_cli_node_store::NodeIds::new(Arc::new(FixedClock(
                1_000,
            )))),
        )
        .unwrap();
    settings
        .change_project_rules(
            &workspace(),
            ProjectRulesChange {
                behavior: RuleBehavior::Deny,
                add: vec![PermissionRule {
                    tool: rust_cli_kernel_api::tool_names::WRITE.into(),
                    content: None,
                }],
                remove: vec![],
            },
        )
        .await
        .unwrap();
    let handle = kernel.open_session(&id).await.unwrap();
    let mut events = handle.subscribe(Some(0));
    handle.send(submit("go", "run the child")).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.next().await {
            if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let requests = model.child.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .any(|content| matches!(
                &content.block,
                ContentBlock::ToolResult { is_error: true, .. }
            ))
    );
    handle.close(EndReason::Other).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
#[expect(clippy::disallowed_methods, reason = "云端删除测试读取独立 PG 连接")]
async fn deleting_a_running_session_stops_it_before_soft_delete() {
    let database = PgDatabase::connect_loopback_test(
        &RedactedString::new(
            std::env::var(rust_cli_base::names::env::ZCODE_TEST_POSTGRES_URL).unwrap(),
        ),
        4,
    )
    .await
    .unwrap();
    let tenant = format!("delete-running-{}", std::process::id());
    let waiting = Arc::new(WaitingModel {
        entered: Notify::new(),
        release: Arc::new(Notify::new()),
        requests: Mutex::new(Vec::new()),
    });
    let kernel = host(
        &database,
        &tenant,
        waiting.clone(),
        Arc::new(SequentialIds::default()),
    );
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
    let handle = kernel.open_session(&id).await.unwrap();
    handle.send(submit("pending", "wait")).await;
    waiting.entered.notified().await;
    tokio::time::timeout(Duration::from_secs(10), kernel.delete_session(&id))
        .await
        .unwrap()
        .unwrap();
    assert!(handle.view().await.is_err());
    assert!(kernel.open_session(&id).await.is_err());
    assert!(kernel.session_snapshot(&id).await.is_err());
    kernel.delete_session(&id).await.unwrap();
}
