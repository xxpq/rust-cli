//! 远端机器上的调用（remote-exec.md §20.8）：相对路径规则按那台机器的工作目录，本机的附加目录不带过去，只读判定看那台
//! 机器的路径风格。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::machine::MachineEnv;

use super::*;
use crate::shell::readonly::is_read_only_on;

/// 路由后的上下文：dev-a 上，工作区是它的工作目录。
fn remote(mode: PermissionMode) -> ToolUseContext {
    let mut context = context(mode);
    context.machine = MachineEnv::new(
        Arc::new(UnusedEnvironment),
        Arc::new(rust_cli_kernel_api::mcp::NoMcpTransport),
    )
    .named(rust_cli_kernel_api::ids::MachineName::new("dev-a"));
    context.workspace = WorkspaceRef {
        environment: EnvironmentId::new("dev-a"),
        root: EnvPath::new("/home/dev/proj"),
    };
    context
}

async fn edit(gate: &PolicyGate, context: &ToolUseContext, path: &str) -> PermissionDecision {
    gate.decide(
        &Named("Edit", PermissionDecision::Passthrough),
        &json!({"file_path": path, "old_string": "a", "new_string": "b"}),
        context,
    )
    .await
}

#[tokio::test]
async fn relative_rules_follow_the_machine_working_directory() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Edit(src/**)")]));
    let mode = PermissionMode::Default;
    // dev-a 上：`src/**` 锚在 /home/dev/proj。
    assert_eq!(
        edit(&gate, &remote(mode), "/home/dev/proj/src/a.rs").await,
        allow()
    );
    // 同一个路径在本机（工作区 /w）不在规则里。
    assert!(matches!(
        edit(&gate, &context(mode), "/home/dev/proj/src/a.rs").await,
        PermissionDecision::Ask { .. }
    ));
}

/// 附加目录是会话的状态，随调用上下文来（session-dirs.md §3.3）：本机的调用按它放行，远端机器上的调用不带过去。
#[tokio::test]
async fn local_additional_directories_do_not_reach_a_remote_machine() {
    let gate = PolicyGate::new(rules(&[]));
    let mode = PermissionMode::AcceptEdits;
    let with_extra = |mut context: ToolUseContext| {
        context.additional_directories = vec![EnvPath::new("/extra")];
        context
    };
    assert!(matches!(
        edit(&gate, &context(mode), "/extra/a.txt").await,
        PermissionDecision::Ask { .. }
    ));
    assert_eq!(
        edit(&gate, &with_extra(context(mode)), "/extra/a.txt").await,
        allow()
    );
    assert!(matches!(
        edit(&gate, &with_extra(remote(mode)), "/extra/a.txt").await,
        PermissionDecision::Ask { .. }
    ));
    // 远端自己的工作目录里照常放行。
    assert_eq!(
        edit(&gate, &remote(mode), "/home/dev/proj/a.txt").await,
        allow()
    );
}

#[test]
fn the_read_only_check_uses_the_machine_path_style() {
    // xargs：Windows 的路径风格下不判只读（原来是本机的 `cfg!(windows)`）。
    assert!(is_read_only_on("xargs echo", false, PathStyle::Posix));
    assert!(!is_read_only_on("xargs echo", false, PathStyle::Windows));
}
