//! 外部账号层的格子（docs/specs/rust-cli/platform.md §12.1）：内容变了才换代，第一份到之前等的一方一直等。
use super::*;

/// 一份内置配置的摘要（测试值）。
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn layer(grants: AccountGrants) -> AccountLayer {
    AccountLayer {
        builtin_digest: DIGEST.to_owned(),
        grants,
    }
}

fn start_plan(entitled: bool) -> AccountLayer {
    layer(AccountGrants::from([(
        "account:zai-start-plan".to_owned(),
        AccountGrant {
            entitled,
            current: Some(true),
            models: Some(vec![rust_cli_base::models::GLM_5_2_ACCOUNT.to_owned()]),
        },
    )]))
}

#[test]
fn nothing_has_arrived_before_the_first_set() {
    assert_eq!(ExternalAccounts::new().current(), None);
}

#[test]
fn only_a_different_content_makes_a_new_version() {
    let accounts = ExternalAccounts::new();
    assert!(accounts.set(start_plan(true)));
    let first = accounts.current().unwrap();
    assert_eq!(first.version, 1);
    assert_eq!(*first.layer, start_plan(true));
    // 同内容的推送不换代：平台层不重建目录
    assert!(!accounts.set(start_plan(true)));
    assert_eq!(accounts.current().unwrap().version, 1);
    assert!(accounts.set(start_plan(false)));
    assert_eq!(accounts.current().unwrap().version, 2);
    // 空的一份也是一份（全部不授权）
    assert!(accounts.set(layer(AccountGrants::new())));
    assert_eq!(accounts.current().unwrap().version, 3);
}

/// 账号事实相同、依据的内置配置不同（22 §15）：是新的一份，平台层要重新比摘要。
#[test]
fn a_different_builtin_digest_makes_a_new_version() {
    let accounts = ExternalAccounts::new();
    assert!(accounts.set(start_plan(true)));
    let mut next = start_plan(true);
    next.builtin_digest = "f".repeat(DIGEST.len());
    assert!(accounts.set(next.clone()));
    let facts = accounts.current().unwrap();
    assert_eq!(facts.version, 2);
    assert_eq!(*facts.layer, next);
}

#[tokio::test]
async fn waiting_for_the_first_copy_ends_when_it_is_set() {
    let accounts = ExternalAccounts::new();
    let copy = accounts.clone();
    let waiting = tokio::spawn(async move { copy.arrived().await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    accounts.set(start_plan(true));
    waiting.await.unwrap();
    // 已经有了就立即返回
    accounts.arrived().await;
}

#[tokio::test]
async fn a_declined_layer_ends_the_wait_without_facts() {
    let accounts = ExternalAccounts::new();
    let copy = accounts.clone();
    let waiting = tokio::spawn(async move { copy.arrived().await });
    // 宿主不下发（握手里没有这项能力）：等的一方不再等，账号 provider 一律不授权
    accounts.decline();
    waiting.await.unwrap();
    assert_eq!(accounts.current(), None);
}

/// 等一份更新的（缺省模型跟着账号层，platform.md §12.2）：已经有更新的立即返回；同内容的推送不算；连续几次换代只看到最后一次。
#[tokio::test]
async fn changed_after_waits_for_a_newer_layer() {
    let accounts = ExternalAccounts::new();
    accounts.set(layer(AccountGrants::new()));
    assert_eq!(accounts.changed_after(0).await, 1);
    let waiting = {
        let accounts = accounts.clone();
        tokio::spawn(async move { accounts.changed_after(1).await })
    };
    tokio::task::yield_now().await;
    accounts.set(layer(AccountGrants::new()));
    tokio::task::yield_now().await;
    assert!(
        !waiting.is_finished(),
        "the same content is not a newer layer"
    );
    let grant = AccountGrant {
        entitled: true,
        current: None,
        models: None,
    };
    accounts.set(layer(AccountGrants::from([(
        "a".to_owned(),
        grant.clone(),
    )])));
    accounts.set(layer(AccountGrants::from([("b".to_owned(), grant)])));
    assert_eq!(waiting.await.unwrap(), 3);
}
