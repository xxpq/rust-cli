use std::borrow::Cow;

use rust_cli_kernel_api::agents::StoredCall;
use rust_cli_kernel_api::ids::ToolUseId;
use serde_json::{Value, json};

use super::{ChildOutcome, SubagentCall, call, calls, outcome};
use crate::row::WithParts;

/// 本工程写的：结果是给模型的文字，关联在 `state.metadata`（node-store.md §4.6 第 6 条）。
fn ours() -> Value {
    json!({
        "type": "tool", "tool": "Agent", "callID": "toolu_1",
        "state": {
            "status": "completed",
            "input": {"description": " look around ", "prompt": "go", "subagent_type": "Explore"},
            "output": "found it",
            "metadata": {"childSessionId": "sess_subagent_agent_1", "agentId": "agent_1", "agentType": "Explore"},
            "time": {"start": 10, "end": 20}
        }
    })
}

#[test]
fn a_call_we_wrote_links_through_its_metadata() {
    assert_eq!(
        call(&ours()),
        Some(SubagentCall {
            tool_use: ToolUseId::new("toolu_1"),
            child: "sess_subagent_agent_1".to_owned(),
            agent: Some("agent_1".to_owned()),
            agent_type: Some("Explore".to_owned()),
            description: Some("look around".to_owned()),
            call: StoredCall::Completed,
            started_at_ms: Some(10),
            ended_at_ms: Some(20),
            error: None,
        })
    );
}

/// Node 写的：metadata 里没有关联，后台回执的文字带 `agentId:` 一行，子会话按 id 推出。
#[test]
fn a_call_node_wrote_links_through_the_agent_id_line() {
    let part = json!({
        "type": "tool", "tool": "Task", "callID": "toolu_2",
        "state": {
            "status": "completed",
            "input": {"prompt": "dig"},
            "output": "Async agent launched successfully.\nagentId: agent_9 (internal ID - do not mention)\n",
            "metadata": {"schemaVersion": 1},
            "time": {"start": 1, "end": 2}
        }
    });
    let found = call(&part).unwrap();
    assert_eq!(found.child, "sess_subagent_agent_9");
    assert_eq!(found.agent.as_deref(), Some("agent_9"));
    assert_eq!(found.agent_type, None);
    assert_eq!(found.description.as_deref(), Some("dig"));
}

#[test]
fn failed_and_unfinished_calls_keep_their_outcome_and_other_tools_are_not_calls() {
    let mut failed = ours();
    failed["state"]["status"] = json!("error");
    failed["state"]["error"] = json!("The operation was aborted");
    let found = call(&failed).unwrap();
    assert_eq!(
        found.call,
        StoredCall::Failed {
            error: "The operation was aborted".to_owned()
        }
    );
    assert_eq!(found.error.as_deref(), Some("The operation was aborted"));
    let mut pending = ours();
    pending["state"] = json!({"status": "pending", "input": {}, "metadata": {"childSessionId": "sess_subagent_agent_1"}});
    assert_eq!(call(&pending).unwrap().call, StoredCall::Unfinished);
    let mut bash = ours();
    bash["tool"] = json!("Bash");
    assert_eq!(call(&bash), None);
}

fn message(id: &str, parts: Vec<Value>) -> WithParts {
    WithParts {
        info: json!({"id": id, "role": "assistant"}),
        parts,
    }
}

#[test]
fn the_same_child_twice_keeps_its_first_place_and_the_later_call() {
    let mut again = ours();
    again["callID"] = json!("toolu_3");
    let mut other = ours();
    other["callID"] = json!("toolu_4");
    other["state"]["metadata"]["childSessionId"] = json!("sess_subagent_agent_2");
    let records = [
        message("m1", vec![ours(), other]),
        message("m2", vec![again]),
    ];
    let messages: Vec<Cow<'_, WithParts>> = records.iter().map(Cow::Borrowed).collect();
    let found: Vec<(String, String)> = calls(&messages)
        .into_iter()
        .map(|call| (call.child, call.tool_use.as_str().to_owned()))
        .collect();
    assert_eq!(
        found,
        [
            ("sess_subagent_agent_1".to_owned(), "toolu_3".to_owned()),
            ("sess_subagent_agent_2".to_owned(), "toolu_4".to_owned()),
        ]
    );
}

#[test]
fn the_child_outcome_is_its_last_answer_or_its_error() {
    let mut last = message(
        "m2",
        vec![
            json!({"type": "text", "text": " first "}),
            json!({"type": "text", "text": "hidden", "ignored": true}),
            json!({"type": "text", "text": "second"}),
        ],
    );
    last.info["time"] = json!({"completed": 99});
    let child = [
        message("m1", vec![json!({"type": "text", "text": "older"})]),
        WithParts {
            info: json!({"id": "u1", "role": "user"}),
            parts: vec![],
        },
        last,
    ];
    assert_eq!(
        outcome(&child),
        ChildOutcome {
            summary: Some("first\n\nsecond".to_owned()),
            completed_at_ms: Some(99),
        }
    );
    let mut failed = message("m3", vec![]);
    failed.info["error"] = json!({"name": "APIError", "data": {"message": "rate limited"}});
    assert_eq!(outcome(&[failed]).summary.as_deref(), Some("rate limited"));
    assert_eq!(outcome(&[]), ChildOutcome::default());
}
