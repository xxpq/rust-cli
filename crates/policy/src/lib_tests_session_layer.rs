//! 会话级的规则层（docs/specs/rust-cli/permission-updates.md §3）：调用上下文带来的规则接在快照之后参与判决（`decide`、
//! `check_rules`），来源说明是 `current session`。修前判决链只看快照，hook 给本会话的规则不生效。

use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::permission::syntax::parse_rule;
use rust_cli_kernel_api::permission::{RuleSource, SourcedRule};

use super::*;

fn layer(entries: &[(RuleBehavior, &str)]) -> Vec<SourcedRule> {
    entries
        .iter()
        .map(|(behavior, text)| SourcedRule {
            source: RuleSource::Session,
            behavior: *behavior,
            rule: parse_rule(text),
        })
        .collect()
}

async fn bash_with(
    gate: &PolicyGate,
    mode: PermissionMode,
    command: &str,
    session_rules: Vec<SourcedRule>,
) -> PermissionDecision {
    let context = ToolUseContext {
        session_rules,
        ..context(mode)
    };
    gate.decide(
        &Named("Bash", PermissionDecision::Passthrough),
        &json!({ "command": command }),
        &context,
    )
    .await
}

#[tokio::test]
async fn session_rules_allow_deny_and_ask() {
    let gate = PolicyGate::default();
    let rules = layer(&[
        (RuleBehavior::Allow, "Bash(touch x)"),
        (RuleBehavior::Deny, "Bash(rm:*)"),
        (RuleBehavior::Ask, "Bash(git push:*)"),
    ]);
    assert_eq!(
        bash_with(&gate, PermissionMode::DontAsk, "touch x", rules.clone()).await,
        allow()
    );
    let denied = bash_with(&gate, PermissionMode::Default, "rm -f a", rules.clone()).await;
    assert!(
        matches!(denied, PermissionDecision::Deny { .. }),
        "{denied:?}"
    );
    let asked = bash_with(&gate, PermissionMode::Default, "git push", rules.clone()).await;
    assert!(ask_message(&asked).contains("current session"), "{asked:?}");
    // 别的会话（上下文里没有这一层）照旧。
    assert!(matches!(
        bash_with(&gate, PermissionMode::DontAsk, "touch x", Vec::new()).await,
        PermissionDecision::Deny { .. }
    ));
}

/// hook 改写的输入过的关卡（`check_rules`）也看会话层。
#[tokio::test]
async fn the_rewrite_gate_sees_session_rules() {
    let gate = PolicyGate::default();
    let context = ToolUseContext {
        session_rules: layer(&[(RuleBehavior::Deny, "Bash(rm:*)")]),
        ..context(PermissionMode::Default)
    };
    let checked = gate
        .check_rules(
            &Named("Bash", PermissionDecision::Passthrough),
            &json!({ "command": "rm -f a" }),
            &context,
        )
        .await;
    assert!(
        matches!(checked, Some(PermissionDecision::Deny { .. })),
        "{checked:?}"
    );
}
