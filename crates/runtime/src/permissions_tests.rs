//! `/permissions` 的列表组成与修改后的快照（tui-manage.md §1.1、§1.4）。
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use std::sync::Arc;

use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::testing::MemoryStore;

use super::*;

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

fn sourced(source: RuleSource, behavior: RuleBehavior, text: &str) -> SourcedRule {
    SourcedRule {
        source,
        behavior,
        rule: parse_rule(text),
    }
}

fn gate() -> PolicyGate {
    PolicyGate::new(PermissionRules {
        version: 3,
        rules: vec![
            sourced(RuleSource::UserSettings, RuleBehavior::Deny, "WebFetch"),
            sourced(RuleSource::LocalSettings, RuleBehavior::Allow, "Glob"),
            sourced(
                RuleSource::CliArg,
                RuleBehavior::Allow,
                "Bash(git status:*)",
            ),
        ],
        ..PermissionRules::default()
    })
}

fn texts(rules: &[SourcedRule]) -> Vec<(RuleSource, RuleBehavior, String)> {
    rules
        .iter()
        .map(|rule| (rule.source, rule.behavior, format_rule(&rule.rule)))
        .collect()
}

async fn add(store: &MemoryStore, behavior: RuleBehavior, text: &str) {
    store
        .change_project_rules(
            &workspace(),
            ProjectRulesChange {
                behavior,
                add: vec![parse_rule(text)],
                remove: Vec::new(),
            },
        )
        .await
        .unwrap();
}

/// 列表：LocalSettings 从 db 现读（快照里的 Glob 是启动时的旧值），会话规则是会话 actor 的会话层（permission-updates.md §3，
/// 调用方从会话的视图取来）。
#[tokio::test]
async fn the_listing_reads_local_settings_fresh() {
    let gate = PolicyGate::new(PermissionRules {
        version: 3,
        rules: vec![
            sourced(RuleSource::UserSettings, RuleBehavior::Deny, "WebFetch"),
            sourced(RuleSource::LocalSettings, RuleBehavior::Allow, "Glob"),
        ],
        ..PermissionRules::default()
    });
    let store = MemoryStore::default();
    add(&store, RuleBehavior::Allow, "Read").await;
    let query = PermissionRulesQuery {
        workspace: workspace(),
        session: Some(SessionId::new("ses_1")),
    };
    let layer = [sourced(RuleSource::Session, RuleBehavior::Allow, "Edit")];
    let rules = listing(&gate, &store, &query, &layer).await.unwrap();
    assert_eq!(
        texts(&rules),
        [
            (
                RuleSource::UserSettings,
                RuleBehavior::Deny,
                "WebFetch".to_owned()
            ),
            (
                RuleSource::LocalSettings,
                RuleBehavior::Allow,
                "Read".to_owned()
            ),
            (RuleSource::Session, RuleBehavior::Allow, "Edit".to_owned()),
        ]
    );
}

/// “总是允许”落库后（db 里已有规则），判决链读回同一份规则集：快照的 LocalSettings 层换成库值（R1-2）。
#[tokio::test]
async fn a_committed_always_allow_replaces_the_local_layer() {
    let store = Arc::new(MemoryStore::default());
    add(&store, RuleBehavior::Allow, "Bash(npm test:*)").await;
    let gate = gate().with_local_rules(Arc::new(StoredRules(store.clone())));
    gate.local_rules_committed(&workspace()).await;
    let snapshot = gate.rules();
    assert_eq!(snapshot.version, 4);
    assert_eq!(
        texts(&snapshot.rules),
        [
            (
                RuleSource::UserSettings,
                RuleBehavior::Deny,
                "WebFetch".to_owned()
            ),
            (
                RuleSource::LocalSettings,
                RuleBehavior::Allow,
                "Bash(npm test:*)".to_owned()
            ),
            (
                RuleSource::CliArg,
                RuleBehavior::Allow,
                "Bash(git status:*)".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn changes_go_to_the_store_and_replace_the_snapshot() {
    let gate = gate();
    let store = MemoryStore::default();
    let session = SessionId::new("ses_1");
    let request = |change| PermissionRulesChange {
        workspace: workspace(),
        session: Some(session.clone()),
        change,
    };
    change(
        &gate,
        &store,
        None,
        request(RuleChange::Add {
            behavior: RuleBehavior::Allow,
            rules: vec![" Bash(rm *) ".to_owned()],
        }),
    )
    .await
    .unwrap();
    // 快照里的 LocalSettings 换成落库后的规则集（旧的 Glob 不在 db 里，被换掉），版本加一。
    let snapshot = gate.rules();
    assert_eq!(snapshot.version, 4);
    assert_eq!(
        texts(&snapshot.rules),
        [
            (
                RuleSource::UserSettings,
                RuleBehavior::Deny,
                "WebFetch".to_owned()
            ),
            (
                RuleSource::LocalSettings,
                RuleBehavior::Allow,
                "Bash(rm *)".to_owned()
            ),
            (
                RuleSource::CliArg,
                RuleBehavior::Allow,
                "Bash(git status:*)".to_owned()
            ),
        ]
    );
    // 删除 LocalSettings 的允许规则：db 与快照里都没有了。
    let local = sourced(RuleSource::LocalSettings, RuleBehavior::Allow, "Bash(rm *)");
    change(
        &gate,
        &store,
        None,
        request(RuleChange::Remove { rule: local }),
    )
    .await
    .unwrap();
    assert!(
        store
            .project_rules(&workspace())
            .await
            .unwrap()
            .allow
            .is_empty()
    );
    assert_eq!(gate.rules().rules.len(), 2);
    // 命令行规则只在内存里去掉；配置文件规则只读。
    let cli = sourced(
        RuleSource::CliArg,
        RuleBehavior::Allow,
        "Bash(git status:*)",
    );
    change(
        &gate,
        &store,
        None,
        request(RuleChange::Remove { rule: cli }),
    )
    .await
    .unwrap();
    assert_eq!(gate.rules().rules.len(), 1);
    let user = sourced(RuleSource::UserSettings, RuleBehavior::Deny, "WebFetch");
    let refused = change(
        &gate,
        &store,
        None,
        request(RuleChange::Remove { rule: user }),
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("read-only"), "{refused}");
    assert_eq!(gate.rules().rules.len(), 1);
}

/// 删会话层的一条：交给会话 actor（一组权限更新，permission-updates.md §3），不改快照；没有打开的会话时拒绝。
#[tokio::test]
async fn removing_a_session_rule_goes_to_the_session() {
    use rust_cli_kernel_api::permission_updates::{PermissionUpdate, UpdatePlace};
    use rust_cli_kernel_api::session_api::{Ack, Command, CommandEnvelope, CommandResult};
    use rust_cli_kernel_api::testing::HandleOverride;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Command>>);

    #[async_trait::async_trait]
    impl HandleOverride for Recorder {
        fn id(&self) -> &SessionId {
            static ID: std::sync::OnceLock<SessionId> = std::sync::OnceLock::new();
            ID.get_or_init(|| SessionId::new("ses_1"))
        }

        async fn send(&self, envelope: CommandEnvelope) -> Ack {
            self.0.lock().unwrap().push(envelope.command);
            Ack::Accepted {
                result: CommandResult::Done,
            }
        }

        fn subscribe(
            &self,
            _after: Option<u64>,
        ) -> futures_util::stream::BoxStream<
            'static,
            rust_cli_kernel_api::session_api::SequencedEvent,
        > {
            Box::pin(futures_util::stream::pending())
        }

        async fn view(&self) -> Result<rust_cli_kernel_api::session_api::SessionView, KernelError> {
            Err(KernelError::Failed {
                message: "not used".to_owned(),
            })
        }
    }

    let gate = gate();
    let store = MemoryStore::default();
    let edit = sourced(RuleSource::Session, RuleBehavior::Allow, "Edit");
    let request = PermissionRulesChange {
        workspace: workspace(),
        session: Some(SessionId::new("ses_1")),
        change: RuleChange::Remove { rule: edit.clone() },
    };
    let session = Recorder::default();
    let id = rust_cli_kernel_api::ids::CommandId::new("cmd_1");
    change(&gate, &store, Some((&session, id)), request.clone())
        .await
        .unwrap();
    assert_eq!(
        *session.0.lock().unwrap(),
        [Command::UpdatePermissions {
            updates: vec![PermissionUpdate::RemoveRules {
                place: UpdatePlace::Session,
                behavior: RuleBehavior::Allow,
                rules: vec![edit.rule.clone()],
            }],
        }]
    );
    assert_eq!(gate.rules().version, 3);
    let refused = change(&gate, &store, None, request).await.unwrap_err();
    assert!(refused.to_string().contains("no open session"), "{refused}");
}
