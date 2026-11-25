use rust_cli_kernel_api::turn_execution::FallbackRule;

use super::*;

fn rules() -> ExecutionFallback {
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

fn reason(error: &ModelError) -> Option<&str> {
    match error {
        ModelError::ExecutionFellBack { reason, .. } => Some(reason),
        _ => None,
    }
}

fn invalid() -> ModelError {
    ModelError::InvalidRequest {
        message: "m".to_owned(),
    }
}

#[test]
fn the_request_stage_matches_the_business_code() {
    let rules = rules();
    let coded = fell_back(
        Some(&rules),
        Stage::Request(Some("3402".to_owned())),
        invalid(),
    );
    assert_eq!(reason(&coded), Some("expired"));
    let other = fell_back(Some(&rules), Stage::Request(None), invalid());
    assert_eq!(reason(&other), Some("failed"));
    // 原来的错误留在里面（计量按它记）
    assert!(matches!(
        coded,
        ModelError::ExecutionFellBack { error, .. } if *error == invalid()
    ));
}

#[test]
fn an_unavailable_selection_uses_the_unavailable_reason_and_a_missing_credential_the_rules() {
    let rules = rules();
    let unavailable = fell_back(Some(&rules), Stage::Endpoint, invalid());
    assert_eq!(reason(&unavailable), Some("unavailable"));
    let no_credential = ModelError::Authentication {
        message: "m".to_owned(),
    };
    assert_eq!(
        reason(&fell_back(Some(&rules), Stage::Endpoint, no_credential)),
        Some("failed")
    );
}

#[test]
fn cancellation_and_context_overflow_never_fall_back() {
    let rules = rules();
    for error in [
        ModelError::Cancelled,
        ModelError::PromptTooLong {
            message: "m".to_owned(),
        },
    ] {
        assert_eq!(
            fell_back(Some(&rules), Stage::Request(None), error.clone()),
            error
        );
    }
}

#[test]
fn without_rules_the_error_is_unchanged() {
    assert_eq!(fell_back(None, Stage::Endpoint, invalid()), invalid());
}
