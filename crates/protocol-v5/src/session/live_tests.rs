//! 会话列表的 `live` 行（04 §3）：回合在跑时 running，回合结束、交互还在等时按上一轮的结局，交互的 id、种类与工具名。

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use serde_json::json;

use super::*;

fn permission() -> InteractionRequest {
    InteractionRequest::Permission {
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Write".to_owned(),
        input: json!({}),
        message: "write?".to_owned(),
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: None,
        machine: rust_cli_kernel_api::ids::MachineName::primary(),
        once: false,
    }
}

fn asked(request: &InteractionRequest, v: Option<u64>) -> Asked<'_> {
    Asked {
        id: "toolu_1",
        v,
        kind: super::super::interactions::kind(request),
        tool: Some(super::super::interactions::tool_name(request)),
    }
}

#[test]
fn a_running_turn_is_busy_and_names_its_pending_interaction() {
    let request = permission();
    let hot = Hot {
        running: true,
        outcome: None,
        pending: vec![asked(&request, Some(3))],
        background: false,
    };
    let row = serde_json::Value::Object(row(&hot).into_iter().collect());
    assert_eq!(
        row,
        json!({
            "phase": "running",
            "runtime": "busy",
            "hasBackgroundWork": false,
            "pendingCount": 1,
            "pendingInteraction": {"id": "toolu_1", "kind": "permission", "toolName": "Write", "v": 3},
        })
    );
}

#[test]
fn a_finished_turn_still_waiting_on_an_answer_shows_its_outcome() {
    let request = permission();
    let hot = Hot {
        running: false,
        outcome: Some(Outcome::Error),
        pending: vec![asked(&request, None)],
        background: false,
    };
    let row = row(&hot);
    assert_eq!(row["phase"], "error");
    assert_eq!(row["runtime"], "ready");
}
