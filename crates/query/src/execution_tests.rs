use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::orchestration::{QueryRequest, QueryStop};
use rust_cli_kernel_api::testing::{ScriptedModel, selection, text_response};
use rust_cli_kernel_api::turn_execution::{
    ExecutionFallback, FallbackRule, RunExecution, TurnExecution,
};

use crate::run::tests::{parts, request, scope};

fn fast() -> ModelSelection {
    ModelSelection::new("account:zai-highspeed-card", "scripted")
}

fn rules() -> ExecutionFallback {
    ExecutionFallback {
        rules: vec![FallbackRule {
            reason: "highspeed_request_failed".to_owned(),
            provider_code: None,
        }],
        unavailable: "highspeed_selection_unavailable".to_owned(),
    }
}

fn executing(run: &RunExecution) -> QueryRequest {
    QueryRequest {
        execution: Some(run.clone()),
        ..request("hi")
    }
}

fn card() -> RunExecution {
    RunExecution::new(TurnExecution {
        selection: fast(),
        credential: Some("card-1".to_owned()),
        fallback: Some(rules()),
    })
}

async fn run(model: Arc<ScriptedModel>, request: QueryRequest) -> QueryStop {
    let (scope, _sink) = scope(parts(model, Vec::new()), &request, Abort::new());
    crate::run::run(scope, request).await.unwrap()
}

/// 执行的一轮：请求用执行选择，带凭据引用与退回规则（端口据规则单次尝试）。
#[tokio::test]
async fn an_execution_turn_requests_with_the_execution_selection_and_reference() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let card = card();
    assert_eq!(
        run(model.clone(), executing(&card)).await,
        QueryStop::Completed
    );
    let requests = model.requests();
    assert_eq!(requests[0].selection, fast());
    assert_eq!(requests[0].options.execution.as_deref(), Some("card-1"));
    assert_eq!(requests[0].options.fallback, Some(rules()));
}

/// 端口说退回：这一步换成会话的模型重发（不带引用与规则，照常重试），只退一次；运行的执行覆盖标成已退回（子代理据此跟随）。
#[tokio::test]
async fn a_fallback_resends_the_step_with_the_session_model_once() {
    let fell_back = vec![Err(ModelError::ExecutionFellBack {
        reason: "highspeed_request_failed".to_owned(),
        error: Box::new(ModelError::Server {
            status: 503,
            message: "busy".to_owned(),
        }),
    })];
    let model = Arc::new(ScriptedModel::new(vec![fell_back, text_response(&["ok"])]));
    let card = card();
    assert_eq!(
        run(model.clone(), executing(&card)).await,
        QueryStop::Completed
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].selection, selection());
    assert_eq!(requests[1].options.execution, None);
    assert_eq!(requests[1].options.fallback, None);
    assert_eq!(card.active(), None);
}

/// 没有执行覆盖的运行（TUI、`-p`、普通的一轮）不变：请求不带引用与规则。
#[tokio::test]
async fn without_an_execution_the_request_carries_nothing_new() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    assert_eq!(
        run(model.clone(), request("hi")).await,
        QueryStop::Completed
    );
    let requests = model.requests();
    assert_eq!(requests[0].selection, selection());
    assert_eq!(requests[0].options.execution, None);
    assert_eq!(requests[0].options.fallback, None);
}

/// 退回的那一刻发查询事件（session-lifecycle.md §10.48）：原因是命中的规则的原因，只发一次；会话 actor 据它记下这一轮的退回事实。
#[tokio::test]
async fn a_fallback_is_announced_once_with_its_reason() {
    let fell_back = vec![Err(ModelError::ExecutionFellBack {
        reason: "highspeed_card_expired".to_owned(),
        error: Box::new(ModelError::InvalidRequest {
            message: "[3402] card".to_owned(),
        }),
    })];
    let model = Arc::new(ScriptedModel::new(vec![fell_back, text_response(&["ok"])]));
    let card = card();
    let request = executing(&card);
    let (scope, sink) = scope(parts(model, Vec::new()), &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::Completed
    );
    let announced: Vec<_> = sink
        .events()
        .into_iter()
        .filter_map(|event| match event {
            rust_cli_kernel_api::orchestration::QueryEvent::ExecutionFellBack { reason } => {
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(announced, vec!["highspeed_card_expired".to_owned()]);
}

/// 没有执行覆盖的运行不发它（TUI、`-p` 的事件不变）。
#[tokio::test]
async fn without_an_execution_no_fallback_is_announced() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let request = request("hi");
    let (scope, sink) = scope(parts(model, Vec::new()), &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    assert!(!sink.events().iter().any(|event| matches!(
        event,
        rust_cli_kernel_api::orchestration::QueryEvent::ExecutionFellBack { .. }
    )));
}
