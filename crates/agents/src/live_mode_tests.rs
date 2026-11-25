//! 子代理的权限模式跟随父会话此刻的模式（docs/specs/rust-cli/agents.md §4.1.3，修 bug，2026-11-12）。修复前子代理的模式在
//! 派生时定下（父运行**开始时**的模式），回合中途切到完全访问之后，正在跑的子代理照旧每个调用都问。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{AgentId, MessageId, SessionId};
use rust_cli_kernel_api::orchestration::SessionOps;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryStore, ModeCell, SequentialIds};
use serde_json::json;
use tokio::sync::Notify;

use super::*;
use crate::spawn;
use crate::test_support::{ParentRun, harness, running, text};

use PermissionMode::{AcceptEdits, Auto, BypassPermissions, DontAsk, Guarded, Plan};

/// 缺省的权限模式（与 `Default` trait 同名，不导入）。
const ASK: PermissionMode = PermissionMode::Default;

/// 声明了 `permissionMode` 的子代理条目（定义照 Explore，只换声明）。
fn declaring(agent: &str, declared: Option<PermissionMode>) -> crate::live::LiveAgent {
    let entry = running(agent, "sess_root");
    let mut definition = (*entry.definition).clone();
    definition.permission_mode = declared;
    crate::live::LiveAgent {
        definition: Arc::new(definition),
        ..entry
    }
}

fn sink_of(live: &Arc<crate::live::Live>, agent: &str) -> ChildSink {
    ChildSink::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
        live.clone(),
        AgentId::new(agent),
        SessionId::new(format!("sess_subagent_{agent}")),
        MessageId::new("prompt_1"),
        None,
    )
}

async fn mode_of(sink: &ChildSink) -> PermissionMode {
    sink.view().await.unwrap().mode()
}

#[tokio::test]
async fn a_background_subagent_follows_the_parent_switching_to_full_access() {
    let gate = Arc::new(Notify::new());
    let harness = harness(Some(gate.clone()));
    harness.ops.mode.set(DontAsk);
    let output = harness
        .call(
            "sess_parent",
            "Agent",
            json!({"description": "Long job", "prompt": "Work.", "run_in_background": true}),
        )
        .await;
    assert!(text(&output).starts_with("Async agent launched successfully."));
    // 父回合早已结束，子代理还在跑；这时把父会话切到完全访问。
    harness.started(1).await;
    harness.ops.mode.set(BypassPermissions);
    gate.notify_one();
    let viewed = harness.viewed(1).await;
    assert_eq!(viewed[0].1, Some(BypassPermissions), "{viewed:?}");
}

#[tokio::test]
async fn a_foreground_subagent_follows_the_parent_switching_mid_call() {
    let gate = Arc::new(Notify::new());
    let harness = harness(Some(gate.clone()));
    harness.ops.mode.set(ASK);
    let call = harness.call(
        "sess_parent",
        "Agent",
        json!({"description": "Long job", "prompt": "Work.", "run_in_background": false}),
    );
    let switch = async {
        harness.started(1).await;
        harness.ops.mode.set(AcceptEdits);
        gate.notify_one();
    };
    let (output, ()) = tokio::join!(call, switch);
    assert!(!output.is_error, "{}", text(&output));
    let viewed = harness.viewed(1).await;
    assert_eq!(viewed[0].1, Some(AcceptEdits), "{viewed:?}");
}

#[tokio::test]
async fn without_an_answer_the_subagent_keeps_the_mode_at_the_agent_call() {
    let gate = Arc::new(Notify::new());
    let harness = harness(Some(gate.clone()));
    // 父会话没有回答（同父会话已经不在）。父运行开始时是 dontAsk，Agent 调用之前已经切到自动编辑：用调用那一刻的。
    harness
        .call_with(
            "sess_parent",
            "Agent",
            json!({"description": "Long job", "prompt": "Work.", "run_in_background": true}),
            ParentRun {
                mode: Some(AcceptEdits),
                ..ParentRun::default()
            },
        )
        .await;
    harness.started(1).await;
    gate.notify_one();
    let viewed = harness.viewed(1).await;
    assert_eq!(viewed[0].1, Some(AcceptEdits), "{viewed:?}");
}

#[test]
fn the_declared_mode_follows_the_lenient_parent_rule() {
    let cases = [
        // 没有声明：父会话此刻的模式。
        (ASK, None, ASK),
        (BypassPermissions, None, BypassPermissions),
        (Plan, None, Plan),
        // 父会话不在宽松的模式：用声明的。
        (ASK, Some(Plan), Plan),
        (DontAsk, Some(AcceptEdits), AcceptEdits),
        (Plan, Some(AcceptEdits), AcceptEdits),
        (ASK, Some(Auto), Auto),
        // 父会话在宽松的模式（完全访问、自动编辑、自动、自主）：仍用父会话的。
        (BypassPermissions, Some(Plan), BypassPermissions),
        (AcceptEdits, Some(Plan), AcceptEdits),
        (Auto, Some(ASK), Auto),
        (Guarded, Some(Plan), Guarded),
        // 声明的完全访问只在父会话也是完全访问时生效。
        (ASK, Some(BypassPermissions), ASK),
        (AcceptEdits, Some(BypassPermissions), AcceptEdits),
        (
            BypassPermissions,
            Some(BypassPermissions),
            BypassPermissions,
        ),
    ];
    for (parent, declared, expected) in cases {
        assert_eq!(
            spawn::mode(parent, declared),
            expected,
            "parent {parent:?}, declared {declared:?}"
        );
    }
}

#[tokio::test]
async fn the_view_recomputes_the_declared_mode_each_time() {
    let parent = Arc::new(ModeCell::default());
    parent.set(ASK);
    let live = Arc::new(crate::live::Live::default());
    live.insert(crate::live::LiveAgent {
        follow: Some(Follow(parent.clone())),
        ..declaring("agent_1", Some(Plan))
    });
    let sink = sink_of(&live, "agent_1");
    assert_eq!(mode_of(&sink).await, Plan);
    parent.set(BypassPermissions);
    assert_eq!(mode_of(&sink).await, BypassPermissions);
}

#[tokio::test]
async fn a_nested_subagent_follows_the_root_through_its_parent() {
    let root = Arc::new(ModeCell::default());
    root.set(ASK);
    let live = Arc::new(crate::live::Live::default());
    live.insert(crate::live::LiveAgent {
        follow: Some(Follow(root.clone())),
        ..declaring("agent_1", None)
    });
    // 孙代理派生时取的是子代理的入口（子代理里 Agent 调用的 `session_ops` 是子代理的输出端）。
    let child = sink_of(&live, "agent_1").live_mode().map(Follow);
    assert!(child.is_some());
    live.insert(crate::live::LiveAgent {
        follow: child,
        ..declaring("agent_2", Some(Plan))
    });
    let grandchild = sink_of(&live, "agent_2");
    assert_eq!(mode_of(&grandchild).await, Plan);
    root.set(AcceptEdits);
    assert_eq!(mode_of(&grandchild).await, AcceptEdits);
    // 孙代理的入口同样一层层接到根会话。
    let handle = grandchild.live_mode().unwrap();
    assert_eq!(handle.mode().await, Some(AcceptEdits));
}

#[tokio::test]
async fn without_an_answer_the_view_uses_the_mode_at_spawn() {
    let silent = Arc::new(ModeCell::default());
    let live = Arc::new(crate::live::Live::default());
    live.insert(crate::live::LiveAgent {
        mode: DontAsk,
        follow: Some(Follow(silent)),
        ..declaring("agent_1", None)
    });
    live.insert(crate::live::LiveAgent {
        mode: AcceptEdits,
        ..declaring("agent_2", None)
    });
    assert_eq!(mode_of(&sink_of(&live, "agent_1")).await, DontAsk);
    assert_eq!(mode_of(&sink_of(&live, "agent_2")).await, AcceptEdits);
}
