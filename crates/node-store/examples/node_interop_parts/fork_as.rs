//! 互操作的第三十四段：v5 的 fork（FORK-V5，docs/specs/rust-cli/session-lifecycle.md §10.57）。
//!
//! `fork-as-write <storage-dir>`：两轮（第一轮里 ExitPlanMode 拿出一份计划，计划随结果落成本工程的 entry），从第一轮的回答分到调用方给的 id（v5 的
//! `turn.fork` 由窗口生成新会话的 id），再用同一个 id 分一次（重发）。输出父会话、新会话、两次的回执、计划所在的调用与第一轮的两条消息。
use rust_cli_kernel_api::store::{ForkReceipt, SessionFork, entry_kinds};

use super::*;

/// 调用方给的新会话 id（v5 的窗口按 Node 的 `sess_` 写法生成）。
const CHILD: &str = "sess_interop_fork_as_child";
/// ExitPlanMode 的调用与它的计划（`rust-cli/plan`，键是调用 id）。
const PLAN_CALL: &str = "toolu_interop_plan";
const PLAN_TEXT: &str = "# Plan\n\n1. Fork it.\n";
const PLAN_PATH: &str = "/w/interop/.zcode/plans/interop.md";

fn receipt(receipt: ForkReceipt) -> &'static str {
    match receipt {
        ForkReceipt::Created { .. } => "created",
        ForkReceipt::Existing => "existing",
    }
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let parent = create(&store).await?;
    let (u1, a1, a2, u2, a3) = (
        message_id(),
        message_id(),
        message_id(),
        message_id(),
        message_id(),
    );
    let plan = Fact::Entry {
        kind: entry_kinds::PLAN.to_owned(),
        id: PLAN_CALL.to_owned(),
        data: json!({"plan": PLAN_TEXT, "path": PLAN_PATH}),
    };
    for (command, facts) in [
        (Some("cmd_f1"), vec![user(&u1, "plan the fork")]),
        (
            None,
            vec![reply(
                &a1,
                vec![call(PLAN_CALL, "ExitPlanMode", &json!({}))],
                StopReason::ToolUse,
            )],
        ),
        (
            None,
            vec![result(&a1, PLAN_CALL, vec![said("approved")], false), plan],
        ),
        (
            None,
            vec![reply(&a2, vec![text("planned")], StopReason::EndTurn)],
        ),
        (Some("cmd_f2"), vec![user(&u2, "second")]),
        (
            None,
            vec![reply(&a3, vec![text("two")], StopReason::EndTurn)],
        ),
    ] {
        store.commit(&parent, batch(command, facts)).await?;
    }
    let loaded = store.load(&parent).await?;
    let fork = || SessionFork {
        parent: parent.clone(),
        child: SessionId::new(CHILD),
        created_at_ms: now_ms(),
        through: a2.clone(),
        selection: loaded
            .model
            .clone()
            .unwrap_or(ModelSelection::new("p", "m")),
        mode: loaded.mode.unwrap_or(PermissionMode::Default),
        plan: loaded.plan,
    };
    let first = receipt(store.fork(fork()).await?);
    let again = receipt(store.fork(fork()).await?);
    print(&json!({
        "parent": parent.as_str(),
        "child": CHILD,
        "receipts": [first, again],
        "planCall": PLAN_CALL,
        "plan": PLAN_TEXT,
        "firstTurn": [u1.as_str(), a2.as_str()],
    }))
}
