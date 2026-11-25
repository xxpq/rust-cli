//! 子代理的询问交给谁（agents.md §4.1.1）：前台经父工具调用，后台经父会话的会话级入口，都没有时没人回答；
//! 三种询问都写上发起它的子代理（`AskedBy`，agents.md §4.1.1），外层转发时不改。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{AgentId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{
    AskedBy, InteractionRequest, Interactions, PermissionAnswer,
};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedInteractions, SequentialIds};
use serde_json::json;
use tokio::sync::mpsc;

use super::{Asks, ChildSink, Parent};
use crate::live::Live;

fn sink(parent: Option<Parent>, asks: Option<Asks>) -> ChildSink {
    ChildSink::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
        Arc::new(Live::default()),
        AgentId::new("agent_1"),
        SessionId::new("sess_subagent_agent_1"),
        MessageId::new("prompt_1"),
        parent,
    )
    .asking(me(), asks)
}

/// 这个子代理（`sink()` 建的那一个）。
fn me() -> AskedBy {
    AskedBy {
        agent: AgentId::new("agent_1"),
        agent_type: "general-purpose".to_owned(),
        session: SessionId::new("sess_subagent_agent_1"),
        parent: SessionId::new("sess_main"),
        description: "Run the fixture".to_owned(),
        tool_use: Some(ToolUseId::new("toolu_agent")),
    }
}

/// 更里层的子代理（它转发上来的询问已经带着来源）。
fn inner() -> AskedBy {
    AskedBy {
        agent: AgentId::new("agent_2"),
        agent_type: "Explore".to_owned(),
        session: SessionId::new("sess_subagent_agent_2"),
        parent: SessionId::new("sess_subagent_agent_1"),
        description: "Look around".to_owned(),
        tool_use: None,
    }
}

fn permission(asked_by: Option<AskedBy>) -> InteractionRequest {
    InteractionRequest::Permission {
        machine: Default::default(),
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "touch a"}),
        message: "ask".to_owned(),
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: asked_by.map(Box::new),
        once: false,
    }
}

#[tokio::test]
async fn a_background_agent_asks_through_the_session_entry_with_its_name() {
    let entry = Arc::new(ScriptedInteractions::allowing());
    let sink = sink(None, Some(Asks(entry.clone())));
    let answer = sink.ask(permission(None)).await.unwrap();
    assert_eq!(answer.answer.decision(), "allow");
    let asked = entry.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].asked_by(), Some(&me()));
    // 撤回也经同一个入口；子代理里再派生的后台子代理拿到同一个入口。
    sink.cancel_ask(&ToolUseId::new("toolu_1")).await;
    assert_eq!(entry.withdrawn(), vec![ToolUseId::new("toolu_1")]);
    let lasting = sink.lasting().unwrap();
    lasting.ask(permission(Some(inner()))).await.unwrap();
    assert_eq!(entry.asked().len(), 2);
}

#[tokio::test]
async fn the_innermost_agent_keeps_its_name_when_forwarded() {
    let parent = Arc::new(ScriptedInteractions::allowing());
    let (progress, _updates) = mpsc::channel(4);
    let sink = sink(
        Some(Parent {
            session: SessionId::new("sess_main"),
            asks: parent.clone(),
            ops: Arc::new(rust_cli_kernel_api::testing::NoSession),
            progress,
        }),
        None,
    );
    sink.ask(permission(Some(inner()))).await.unwrap();
    let asked = parent.asked();
    assert_eq!(asked[0].asked_by(), Some(&inner()));
    // 前台子代理没有自己的入口：会话级入口来自父工具调用（这个替身不给）。
    assert!(sink.lasting().is_none());
}

/// 没有入口的后台子代理（`-p`）不询问：权限按“没有询问的入口”回答，管线据此给拒绝的文字（agents.md §4.1.1）。
#[tokio::test]
async fn without_an_entry_a_background_agent_is_not_asked() {
    let sink = sink(None, None);
    let answer = sink.ask(permission(None)).await.unwrap();
    assert_eq!(answer.answer, PermissionAnswer::NoPrompts.into());
    assert!(sink.lasting().is_none());
}

/// 修 bug（19-accept.md §15.3 D8）：以前只给权限询问写子代理的名字，提问与计划审批转发到父会话时没有来源，
/// App 的问答卡片上看不出是子代理在问。现在三种询问都写，与 Node 的子代理询问代理相同。
#[tokio::test]
async fn questions_and_plan_approvals_are_marked_too() {
    let entry = Arc::new(ScriptedInteractions::allowing());
    let sink = sink(None, Some(Asks(entry.clone())));
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_q"),
        questions: json!([]),
        asked_by: None,
    };
    let plan = InteractionRequest::PlanApproval {
        tool_use: ToolUseId::new("toolu_p"),
        plan: "do it".to_owned(),
        asked_by: None,
    };
    sink.ask(question).await.unwrap();
    sink.ask(plan).await.unwrap();
    let asked = entry.asked();
    assert_eq!(asked.len(), 2);
    for request in &asked {
        assert_eq!(request.asked_by(), Some(&me()), "{request:?}");
    }
}
