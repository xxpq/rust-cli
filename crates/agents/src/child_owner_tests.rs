//! 前台子代理把会话级请求转给父会话；后台子代理没有父调用（docs/specs/rust-cli/tools-more.md §10.2）。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::{AgentId, MessageId, SessionId};
use rust_cli_kernel_api::orchestration::{Interactions, OrchestrationError};
use rust_cli_kernel_api::testing::{MemoryStore, SequentialIds};
use tokio::sync::mpsc;

use super::*;

/// 父会话：顶层会话是 `sess_root`，记下问到的会话、收到的工作目录与撤回的询问。
#[derive(Default)]
struct Root(std::sync::Mutex<Vec<String>>);

impl Root {
    fn note(&self, line: String) {
        self.0.lock_unpoisoned().push(line);
    }
}

/// 只记下撤回（询问没有人回答）。
#[async_trait]
impl rust_cli_kernel_api::testing::AskOverride for Root {
    async fn cancel_ask(&self, tool_use: &rust_cli_kernel_api::ids::ToolUseId) {
        self.note(format!("cancel {tool_use}"));
    }
}

#[async_trait]
impl rust_cli_kernel_api::testing::OpsOverride for Root {
    async fn owner_session(&self, session: &SessionId) -> SessionId {
        self.note(format!("owner of {session}"));
        SessionId::new("sess_root")
    }

    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError> {
        self.note(format!("workspace {}", root.as_str()));
        Ok(())
    }

    async fn subagent_started(&self, start: rust_cli_kernel_api::subagent_runs::SubagentStart) {
        self.note(format!("subagent {}", start.agent));
    }
}

fn sink(parent: Option<Parent>) -> ChildSink {
    ChildSink::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
        Arc::default(),
        AgentId::new("agent_1"),
        SessionId::new("sess_child"),
        MessageId::new("prompt_1"),
        parent,
    )
}

#[tokio::test]
async fn foreground_children_forward_to_the_parent_session() {
    let root = Arc::new(Root::default());
    let (progress, _rx) = mpsc::channel(1);
    let child = sink(Some(Parent {
        session: SessionId::new("sess_parent"),
        asks: root.clone(),
        ops: root.clone(),
        progress,
    }));
    let own = SessionId::new("sess_child");
    assert_eq!(child.owner_session(&own).await, SessionId::new("sess_root"));
    child
        .set_workspace(EnvPath::new("/Users/dev/repo/.zcode/worktrees/a"))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    // 被取消的询问撤回也转给父会话（R1-5）。
    child
        .cancel_ask(&rust_cli_kernel_api::ids::ToolUseId::new("toolu_1"))
        .await;
    assert_eq!(
        *root.0.lock_unpoisoned(),
        [
            "owner of sess_parent",
            "workspace /Users/dev/repo/.zcode/worktrees/a",
            "cancel toolu_1"
        ]
    );
}

/// 子代理里派生的子代理不进父会话的子代理表（session-lifecycle.md §10.56）：前台的子代理也不转给父会话。
#[tokio::test]
async fn a_child_does_not_report_its_own_subagents_to_the_parent() {
    let root = Arc::new(Root::default());
    let (progress, _rx) = mpsc::channel(1);
    let child = sink(Some(Parent {
        session: SessionId::new("sess_parent"),
        asks: root.clone(),
        ops: root.clone(),
        progress,
    }));
    child
        .subagent_started(rust_cli_kernel_api::subagent_runs::SubagentStart {
            tool_use: rust_cli_kernel_api::ids::ToolUseId::new("toolu_nested"),
            agent: AgentId::new("agent_2"),
            child: SessionId::new("sess_subagent_agent_2"),
            agent_type: "Explore".to_owned(),
            description: "Nested look".to_owned(),
            background: false,
        })
        .await;
    assert!(root.0.lock_unpoisoned().is_empty());
}

#[tokio::test]
async fn background_children_own_their_session() {
    let child = sink(None);
    let own = SessionId::new("sess_child");
    assert_eq!(child.owner_session(&own).await, own);
    assert!(
        child
            .set_workspace(EnvPath::new("/Users/dev"))
            .await
            .is_err()
    );
}

/// 父会话的视图：主模型是父会话现在的（`/model` 换过的那一个）。
struct ParentView;

#[async_trait]
impl rust_cli_kernel_api::testing::OpsOverride for ParentView {
    async fn view(&self) -> Option<rust_cli_kernel_api::orchestration::RunView> {
        Some(rust_cli_kernel_api::orchestration::RunView {
            selection: Some(parent_now()),
            session_rules: vec![session_rule("Bash(touch now)")],
            ..rust_cli_kernel_api::orchestration::RunView::new(
                rust_cli_kernel_api::permission::PermissionMode::Default,
                crate::test_support::workspace(),
            )
        })
    }
}

impl rust_cli_kernel_api::testing::AskOverride for ParentView {}

fn parent_now() -> rust_cli_kernel_api::model::ModelSelection {
    rust_cli_kernel_api::model::ModelSelection::new("provider-a", "model-switched")
}

/// 子代理这次运行的模型（派生时定下，在 `Live` 里）。
fn run_model() -> rust_cli_kernel_api::model::ModelSelection {
    rust_cli_kernel_api::model::ModelSelection::new("provider-a", "model-at-spawn")
}

fn session_rule(text: &str) -> rust_cli_kernel_api::permission::SourcedRule {
    rust_cli_kernel_api::permission::SourcedRule {
        source: rust_cli_kernel_api::permission::RuleSource::Session,
        behavior: rust_cli_kernel_api::permission::RuleBehavior::Allow,
        rule: rust_cli_kernel_api::permission::syntax::parse_rule(text),
    }
}

fn with_agent(parent: Option<Parent>) -> ChildSink {
    let live = Arc::new(crate::live::Live::default());
    live.insert(crate::live::LiveAgent {
        selection: run_model(),
        session_rules: vec![session_rule("Bash(touch spawned)")],
        ..crate::test_support::running("agent_1", "sess_root")
    });
    ChildSink::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
        live,
        AgentId::new("agent_1"),
        SessionId::new("sess_child"),
        MessageId::new("prompt_1"),
        parent,
    )
}

/// 修 bug（工作线 HOOKS-2，hooks.md §5.3）：后台子代理的视图以前没有主模型，prompt hook 取不到缺省模型、一律不发请求
/// （App 缺省把子代理放到后台）。后台给它这次运行的模型；前台照旧给父会话现在的。
#[tokio::test]
async fn the_view_names_the_model_for_side_requests() {
    let background = with_agent(None);
    let view = background.view().await;
    assert_eq!(view.and_then(|view| view.selection), Some(run_model()));

    let (progress, _rx) = mpsc::channel(1);
    let parent = Arc::new(ParentView);
    let foreground = with_agent(Some(Parent {
        session: SessionId::new("sess_parent"),
        asks: parent.clone(),
        ops: parent,
        progress,
    }));
    let view = foreground.view().await;
    assert_eq!(view.and_then(|view| view.selection), Some(parent_now()));
}

/// 会话级的规则层（permission-updates.md §3）：前台读父会话现在的，后台用派生时的副本。
#[tokio::test]
async fn the_view_carries_the_session_rules() {
    let background = with_agent(None);
    let view = background.view().await.unwrap();
    assert_eq!(view.session_rules, [session_rule("Bash(touch spawned)")]);

    let (progress, _rx) = mpsc::channel(1);
    let parent = Arc::new(ParentView);
    let foreground = with_agent(Some(Parent {
        session: SessionId::new("sess_parent"),
        asks: parent.clone(),
        ops: parent,
        progress,
    }));
    let view = foreground.view().await.unwrap();
    assert_eq!(view.session_rules, [session_rule("Bash(touch now)")]);
}

/// `-p` 里没有入口的后台子代理（permission-updates.md §5、D7）：hook 的更新里落在本机的规则与目录作为子会话的事实提交，会话层与模式
/// 没有能落的 owner，跳过。
#[tokio::test]
async fn a_background_child_without_asks_keeps_only_local_updates() {
    use rust_cli_kernel_api::ids::TraceId;
    use rust_cli_kernel_api::orchestration::Interactions;
    use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule, RuleBehavior};
    use rust_cli_kernel_api::permission_updates::{PermissionUpdate, UpdatePlace};
    use rust_cli_kernel_api::store::{Fact, SessionKind, SessionMeta, SessionStore};

    let store = Arc::new(MemoryStore::default());
    let child = SessionId::new("sess_child");
    store
        .create(
            SessionMeta {
                id: child.clone(),
                kind: SessionKind::Subagent,
                parent: Some(SessionId::new("sess_root")),
                workspace: crate::test_support::workspace(),
                title: "work".to_owned(),
                trace: TraceId::new("trace"),
                created_at_ms: 1,
            },
            rust_cli_kernel_api::testing::no_facts(),
        )
        .await
        .unwrap();
    let live = Arc::new(crate::live::Live::default());
    live.insert(crate::test_support::running("agent_1", "sess_root"));
    let sink = ChildSink::new(
        store.clone(),
        Arc::new(SequentialIds::default()),
        live,
        AgentId::new("agent_1"),
        child.clone(),
        MessageId::new("prompt_1"),
        None,
    );
    let rule = PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("touch second".to_owned()),
    };
    sink.apply_permission_updates(vec![
        PermissionUpdate::AddRules {
            place: UpdatePlace::Local,
            behavior: RuleBehavior::Allow,
            rules: vec![rule.clone()],
        },
        PermissionUpdate::AddRules {
            place: UpdatePlace::Session,
            behavior: RuleBehavior::Deny,
            rules: vec![rule.clone()],
        },
        PermissionUpdate::SetMode {
            place: UpdatePlace::Local,
            mode: PermissionMode::AcceptEdits,
        },
    ])
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    let permission_facts: Vec<Fact> = store
        .facts(&child)
        .into_iter()
        .filter(|fact| {
            matches!(
                fact,
                Fact::PermissionRulesAdded { .. }
                    | Fact::ExecutionChanged { .. }
                    | Fact::ProjectModeSet { .. }
            )
        })
        .collect();
    assert_eq!(
        permission_facts,
        [Fact::PermissionRulesAdded {
            behavior: RuleBehavior::Allow,
            rules: vec![rule],
        }]
    );
}

/// 子代理的拒绝计数归它这次运行的输出端（auto-classifier.md §16.5、D-C6）：前台也不转给父会话；视图读出当前值。
#[tokio::test]
async fn children_count_their_own_auto_denials() {
    use rust_cli_kernel_api::permission::auto_mode::{AutoVerdict, DenialCounts, DenialLimit};
    let root = Arc::new(Root::default());
    let (progress, _rx) = mpsc::channel(1);
    let live = Arc::new(crate::live::Live::default());
    live.insert(crate::test_support::running("agent_1", "sess_root"));
    let child = ChildSink::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
        live,
        AgentId::new("agent_1"),
        SessionId::new("sess_child"),
        MessageId::new("prompt_1"),
        Some(Parent {
            session: SessionId::new("sess_parent"),
            asks: root.clone(),
            ops: root.clone(),
            progress,
        }),
    );
    let mut limits = Vec::new();
    for _ in 0..3 {
        limits.push(child.note_auto_verdict(AutoVerdict::Blocked).await);
    }
    assert_eq!(limits, [None, None, Some(DenialLimit::Consecutive(3))]);
    let view = child.view().await.unwrap();
    assert_eq!(
        view.auto_denials,
        DenialCounts {
            consecutive: 3,
            total: 3
        }
    );
    assert!(root.0.lock_unpoisoned().is_empty());
}
