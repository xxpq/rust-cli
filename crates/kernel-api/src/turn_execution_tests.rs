use super::*;

fn fallback() -> ExecutionFallback {
    ExecutionFallback {
        rules: vec![
            FallbackRule {
                reason: "expired".to_owned(),
                provider_code: Some("3402".to_owned()),
            },
            FallbackRule {
                reason: "failed".to_owned(),
                provider_code: None,
            },
        ],
        unavailable: "unavailable".to_owned(),
    }
}

fn execution() -> TurnExecution {
    TurnExecution {
        selection: ModelSelection::new("fast", "m"),
        credential: Some("card".to_owned()),
        fallback: Some(fallback()),
    }
}

#[test]
fn rules_match_in_order_and_a_rule_without_a_code_matches_any_failure() {
    let fallback = fallback();
    assert_eq!(fallback.reason(Some("3402")), Some("expired"));
    assert_eq!(fallback.reason(Some("1302")), Some("failed"));
    assert_eq!(fallback.reason(None), Some("failed"));
}

#[test]
fn only_coded_rules_leave_other_failures_alone() {
    let fallback = ExecutionFallback {
        rules: vec![FallbackRule {
            reason: "expired".to_owned(),
            provider_code: Some("3402".to_owned()),
        }],
        unavailable: "unavailable".to_owned(),
    };
    assert_eq!(fallback.reason(None), None);
    assert_eq!(fallback.reason(Some("1302")), None);
}

#[test]
fn a_run_falls_back_once_and_its_clones_see_it() {
    let run = RunExecution::new(execution());
    let seen_by_the_agent_tool = run.clone();
    assert_eq!(run.active(), Some(&execution()));
    assert!(run.fall_back());
    assert!(!run.fall_back(), "only once");
    assert_eq!(seen_by_the_agent_tool.active(), None);
}

#[test]
fn the_execution_round_trips_and_omits_what_is_missing() {
    let bare = TurnExecution {
        credential: None,
        fallback: None,
        ..execution()
    };
    let json = serde_json::to_value(&bare).unwrap();
    assert!(json.get("credential").is_none() && json.get("fallback").is_none());
    let full = execution();
    let back: TurnExecution = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
    assert_eq!(back, full);
}
