//! 一条输入的执行覆盖（docs/specs/rust-cli/session-lifecycle.md §10.39）。

use std::sync::Arc;

use rust_cli_kernel_api::input_extras::{InputExtras, TurnConfig};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::session_api::{Command, Delivery, SessionHandle};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::turn_execution::TurnExecution;

use crate::test_support::{Setup, envelope, finished, until};

fn chosen() -> ModelSelection {
    ModelSelection::new("fixture", "chosen")
}

fn fast() -> ModelSelection {
    ModelSelection::new("account:zai-highspeed-card", "chosen")
}

/// G2：执行覆盖只管这一轮的请求。会话的模型只由这条输入的 `modelSelection`（`TurnConfig`）写，执行选择不写成会话的模型、不落库；
/// 这一轮的请求用执行选择、带凭据引用。以前 v5 的映射把加速选择放进 `modelSelection`，开回合时写成会话的模型并落库，之后不带选择的回合
/// 都用加速 provider。
#[tokio::test]
async fn the_execution_selection_runs_the_turn_but_never_becomes_the_session_model() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    let extras = InputExtras {
        config: TurnConfig {
            selection: Some(Box::new(chosen())),
            ..TurnConfig::default()
        },
        execution: Some(Box::new(TurnExecution {
            selection: fast(),
            credential: Some("card-1".to_owned()),
            fallback: None,
        })),
        ..InputExtras::default()
    };
    built
        .session
        .send(envelope(
            "c1",
            Command::Submit {
                text: "hi".to_owned(),
                extras,
                delivery: Delivery::Queue,
                target: None,
            },
        ))
        .await;
    until(&mut events, finished).await;
    let request = model.requests().pop().unwrap();
    assert_eq!(request.selection, fast());
    assert_eq!(request.options.execution.as_deref(), Some("card-1"));
    assert_eq!(built.session.view().await.unwrap().selection, chosen());
    let selected: Vec<_> = built
        .store
        .facts(built.session.id())
        .into_iter()
        .filter_map(|fact| match fact {
            Fact::ModelSelected { selection } => Some(selection),
            _ => None,
        })
        .collect();
    assert!(
        !selected.is_empty() && selected.iter().all(|selection| *selection != fast()),
        "{selected:?}"
    );
}
