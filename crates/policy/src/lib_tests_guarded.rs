//! guarded 模式与 plan 下的判决（修复线 F20，shell-permissions.md §4.6）：危险命令询问一次、没有建议规则，allow
//! 规则与 hook 放行都不盖过它，deny 规则仍然优先；plan 开着时（判决用的模式是 plan）yolo / guarded 也照 plan 询问。

use super::*;

const DANGER: &str = "Dangerous command requires your approval for this execution";

/// guarded 的危险命令询问：只批准这一次（执行管线不跑 PermissionRequest hooks），没有建议规则。
fn asked_once() -> PermissionDecision {
    PermissionDecision::Ask {
        message: DANGER.to_owned(),
        suggestions: Vec::new(),
        once: true,
    }
}

#[tokio::test]
async fn guarded_asks_once_for_dangerous_commands_and_allows_the_rest() {
    let gate = PolicyGate::default();
    assert_eq!(
        bash(&gate, PermissionMode::Guarded, "rm -rf build").await,
        asked_once()
    );
    // 其余命令同 yolo：不询问。
    assert_eq!(
        bash(&gate, PermissionMode::Guarded, "touch x").await,
        allow()
    );
    assert_eq!(
        bash(&gate, PermissionMode::Guarded, "git push origin main").await,
        allow()
    );
    // yolo 不识别危险命令。
    assert_eq!(
        bash(&gate, PermissionMode::BypassPermissions, "rm -rf build").await,
        allow()
    );
    // 只批准这一次是判决结果的一部分（工作线 F137 以前是单独的 `ask_once`）：别的危险命令同样带它，yolo 下放行。
    assert_eq!(
        bash(&gate, PermissionMode::Guarded, "git reset --hard").await,
        asked_once()
    );
    assert_eq!(
        bash(&gate, PermissionMode::BypassPermissions, "git reset --hard").await,
        allow()
    );
}

/// 危险命令又命中整个工具的 ask 规则：问的是规则那一句，同样只批准这一次（与以前的 `ask_once` 只看命令相同）；
/// 不危险的命令命中它时是普通的询问。
#[tokio::test]
async fn a_whole_tool_ask_rule_on_a_dangerous_command_is_asked_once() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Ask, "Bash")]));
    let dangerous = bash(&gate, PermissionMode::Guarded, "rm -rf build").await;
    assert!(
        matches!(&dangerous, PermissionDecision::Ask { once: true, message, .. } if message != DANGER),
        "{dangerous:?}"
    );
    assert!(matches!(
        bash(&gate, PermissionMode::Guarded, "touch x").await,
        PermissionDecision::Ask { once: false, .. }
    ));
}

#[tokio::test]
async fn allow_rules_do_not_skip_the_guarded_question_but_deny_rules_win() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Allow, "Bash(rm:*)"),
        (RuleBehavior::Deny, "Bash(git reset:*)"),
    ]));
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Guarded, "rm -rf build").await),
        DANGER
    );
    assert!(matches!(
        bash(&gate, PermissionMode::Guarded, "git reset --hard").await,
        PermissionDecision::Deny { .. }
    ));
    // hook 放行时仍然生效的检查也按 guarded 跑（Node user-once 不让 hook 替用户批准）。
    let input = json!({"command": "rm -rf build"});
    let tool = Named("Bash", PermissionDecision::Passthrough);
    assert_eq!(
        gate.check_rules(&tool, &input, &context(PermissionMode::Guarded))
            .await,
        Some(asked_once())
    );
    assert_eq!(
        gate.check_rules(&tool, &input, &context(PermissionMode::BypassPermissions))
            .await,
        None
    );
}

/// plan 开着时 actor 交给判决链的是 plan（ExecutionState::effective），权限模式是 yolo / guarded 也一样：危险命令不走 guarded 的
/// 询问（Node `!planEnabled && …`）。从 yolo / guarded 进入的 plan 不开自动语义（auto-classifier.md §16.6），要询问的动作询问用户；
/// 从 default 进入的交分类器（这里是缺省的“判不了”，拦下）。C2 之前用 `auto_in_plan` 开关，不看从哪个模式进入。
#[tokio::test]
async fn plan_overrides_yolo_and_guarded() {
    use rust_cli_kernel_api::permission::ExecutionState;
    let gate = PolicyGate::default();
    let tool = Named("Bash", PermissionDecision::Passthrough);
    let run = |live: &Arc<super::Live>, command: &str| {
        let context = live.context();
        let input = json!({"command": command});
        let gate = &gate;
        let tool = &tool;
        async move { gate.decide(tool, &input, &context).await }
    };
    for mode in [PermissionMode::BypassPermissions, PermissionMode::Guarded] {
        let state = ExecutionState::from(mode).with_plan(true);
        let live = super::Live::new(state);
        assert!(
            matches!(run(&live, "touch x").await, PermissionDecision::Ask { .. }),
            "{mode:?}"
        );
        assert!(
            matches!(
                run(&live, "rm -rf build").await,
                PermissionDecision::Ask { once: false, .. }
            ),
            "{mode:?}"
        );
        // 关掉 plan 回到原来的权限模式。
        live.set(state.with_plan(false));
        assert_eq!(run(&live, "touch x").await, allow());
    }
    let from_default = super::Live::new(ExecutionState::default().with_plan(true));
    assert_eq!(
        run(&from_default, "touch x").await,
        deny(texts::CLASSIFIER_UNAVAILABLE)
    );
}
