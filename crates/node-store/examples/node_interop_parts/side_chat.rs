//! 互操作的第三十五段：v5 的侧聊（SIDECHAT-V5，docs/specs/rust-cli/session-lifecycle.md §10.58）。
//!
//! `side-chat-write <storage-dir>`：两轮问答的父会话，分出侧聊到调用方给的 id（全部历史），再用同一个 id 分一次（重发）。输出父会话、侧聊与两次的回执。
use rust_cli_kernel_api::store::{ForkReceipt, SideChatBase, SideChatFork};

use super::*;

/// 调用方给的侧聊 id（v5 的窗口生成）。
const CHILD: &str = "sess_interop_side_chat";

fn receipt(receipt: ForkReceipt) -> &'static str {
    match receipt {
        ForkReceipt::Created { .. } => "created",
        ForkReceipt::Existing => "existing",
    }
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let parent = create(&store).await?;
    let (u1, a1, u2, a2) = (message_id(), message_id(), message_id(), message_id());
    for (command, facts) in [
        (Some("cmd_s1"), vec![user(&u1, "ask the parent")]),
        (
            None,
            vec![reply(&a1, vec![text("parent answer")], StopReason::EndTurn)],
        ),
        (Some("cmd_s2"), vec![user(&u2, "second")]),
        (
            None,
            vec![reply(&a2, vec![text("two")], StopReason::EndTurn)],
        ),
    ] {
        store.commit(&parent, batch(command, facts)).await?;
    }
    let loaded = store.load(&parent).await?;
    let fork = || SideChatFork {
        parent: parent.clone(),
        child: SessionId::new(CHILD),
        created_at_ms: now_ms(),
        base: SideChatBase::All,
        selection: loaded
            .model
            .clone()
            .unwrap_or(ModelSelection::new("p", "m")),
        mode: loaded.mode.unwrap_or(PermissionMode::Default),
        plan: loaded.plan,
    };
    let first = receipt(store.side_chat(fork()).await?);
    let again = receipt(store.side_chat(fork()).await?);
    print(&json!({
        "parent": parent.as_str(),
        "child": CHILD,
        "receipts": [first, again],
    }))
}
