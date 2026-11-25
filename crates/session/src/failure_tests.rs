//! 结构化的失败与重试时刻（session-lifecycle.md §10.9，K-S6、K-S7）。

use std::sync::Arc;

use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::session_api::{KernelEvent, SessionHandle, TurnOutcome};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};

use crate::test_support::{Setup, finished, submit, until};

/// 以前失败的回合只有文字：前端要解析它才知道是额度用尽还是过载。
#[tokio::test]
async fn a_model_failure_carries_its_cause() {
    let error = ModelError::QuotaExhausted {
        message: "plan limit".to_owned(),
    };
    let model = Arc::new(ScriptedModel::new(vec![vec![Err(error.clone())]]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    let Some(KernelEvent::TurnFinished {
        outcome: TurnOutcome::Failed { message, cause },
        ..
    }) = seen.last()
    else {
        panic!("{seen:?}");
    };
    assert_eq!(cause.as_ref(), Some(&error));
    assert_eq!(*message, error.to_string());
}

/// 重试一次再回答的模型：先出一条重试通知（等 500 ms），再给一段文字。
struct RetryingOnce;

impl rust_cli_kernel_api::model::ModelPort for RetryingOnce {
    fn stream(
        &self,
        _request: rust_cli_kernel_api::model::ModelRequest,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelEvent, ModelError>,
    > {
        use futures_util::StreamExt;
        futures_util::stream::iter(text_response(&["ok"])).boxed()
    }

    fn stream_with_notices(
        &self,
        request: rust_cli_kernel_api::model::ModelRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelStreamItem, ModelError>,
    > {
        use futures_util::StreamExt;
        use rust_cli_kernel_api::model::{ModelNotice, ModelStreamItem};
        let notice = ModelStreamItem::Notice(ModelNotice::Retrying {
            attempt: 1,
            max_retries: 10,
            delay_ms: 500,
            error: ModelError::Overloaded {
                message: "busy".to_owned(),
            },
        });
        futures_util::stream::once(async move { Ok(notice) })
            .chain(
                self.stream(request, cancel)
                    .map(|item| item.map(ModelStreamItem::Event)),
            )
            .boxed()
    }
}

/// 以前重试通知只有相对的 `delay_ms`：前端不知道它从哪一刻算（02 `control.apiRetry.nextRetryAt`）。
#[tokio::test]
async fn a_retry_notice_carries_when_it_was_sent() {
    let built = Setup::new(Arc::new(RetryingOnce)).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    // 测试的时钟固定在 1000（test_support）。
    assert!(
        seen.iter().any(|event| matches!(
            event,
            KernelEvent::ModelNotice {
                at: 1_000,
                notice: rust_cli_kernel_api::model::ModelNotice::Retrying { delay_ms: 500, .. }
            }
        )),
        "{seen:?}"
    );
}
