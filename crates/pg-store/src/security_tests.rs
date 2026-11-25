use crate::StoreAccessContext;
use crate::tests::{batch, meta, project};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};
use rust_cli_kernel_api::store::{
    LocalSettings, ProjectRulesChange, SessionFilter, SessionStore, StoreError,
};
use rust_cli_kernel_api::testing::FixedClock;
use rust_cli_kernel_api::workspace_trust::TrustKey;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn user_trust_project_binding_and_expired_identity_are_isolated() {
    let (_, owner, tenant) = crate::tests::tenant_fixture("security").await;
    let session = SessionId::new("secure");
    owner.create(meta("secure"), batch("first")).await.unwrap();
    let key = TrustKey::new("/workspace");
    owner.trust_workspace(&key).await.unwrap();
    assert_eq!(
        owner
            .trusted_workspace(std::slice::from_ref(&key))
            .await
            .unwrap(),
        Some(key.clone())
    );
    owner
        .change_project_rules(
            &project().workspace,
            ProjectRulesChange {
                behavior: RuleBehavior::Allow,
                add: vec![PermissionRule {
                    tool: "Read".into(),
                    content: Some("/own/*".into()),
                }],
                remove: Vec::new(),
            },
        )
        .await
        .unwrap();
    let mut bob = owner.clone();
    bob.access = StoreAccessContext::verified(&format!("{tenant}-bob"), u64::MAX).unwrap();
    assert!(
        matches!(bob.load(&session).await, Err(StoreError::NotFound { .. })),
        "users must not share session data"
    );
    assert!(matches!(
        bob.rename(&session, "foreign").await,
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        bob.delete(&session).await,
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        bob.commit(&session, batch("foreign")).await,
        Err(StoreError::NotFound { .. })
    ));
    bob.create(meta("secure"), batch("bob-first"))
        .await
        .unwrap();
    bob.delete(&session).await.unwrap();
    assert_eq!(owner.load(&session).await.unwrap().messages.len(), 1);
    let mut renewed = owner.clone();
    renewed.access = StoreAccessContext::verified(&tenant, u64::MAX - 1).unwrap();
    assert!(renewed.load(&session).await.is_ok());
    assert_eq!(
        bob.trusted_workspace(std::slice::from_ref(&key))
            .await
            .unwrap(),
        None
    );
    assert!(
        bob.project_rules(&project().workspace)
            .await
            .unwrap()
            .allow
            .is_empty()
    );
    let mut other_project = owner.clone();
    let mut binding = project();
    binding.id = "different-project-at-the-same-path".into();
    other_project.projects = Arc::new(vec![binding]);
    assert!(matches!(
        other_project.load(&session).await,
        Err(StoreError::NotFound { .. })
    ));
    assert!(
        other_project
            .list(SessionFilter {
                workspace: project().workspace,
                limit: 10
            })
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        other_project
            .trusted_workspace(std::slice::from_ref(&key))
            .await
            .unwrap(),
        None
    );
    assert!(
        other_project
            .project_rules(&project().workspace)
            .await
            .unwrap()
            .allow
            .is_empty()
    );
    assert_eq!(
        owner
            .project_rules(&project().workspace)
            .await
            .unwrap()
            .allow
            .len(),
        1
    );
    let mut expired = owner.clone();
    expired.access = StoreAccessContext::verified(&tenant, 1).unwrap();
    expired.clock = Arc::new(FixedClock(0));
    assert!(
        matches!(
            expired.load(&session).await,
            Err(StoreError::Forbidden { .. })
        ),
        "authorization uses the DB clock"
    );
}
