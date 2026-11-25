//! 命令账本（session-lifecycle.md §10.3，K-A1）：重复的命令回原结果；同 id 不同内容是冲突。
use std::sync::Arc;

use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::session_api::{Ack, Command, CommandResult, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, selection, text_response};
use tokio::sync::Notify;

use crate::test_support::{GatedModel, Setup, envelope, finished, submit, until};

/// 以前重复的命令只回一个不带内容的 `Duplicate`。
#[tokio::test]
async fn a_repeated_command_gets_its_original_result() {
    let built = Setup::new(Arc::new(ScriptedModel::new(Vec::new())))
        .build()
        .await;
    let set = || {
        envelope(
            "c1",
            Command::SetModel {
                selection: selection(),
            },
        )
    };
    let first = built.session.send(set()).await;
    assert!(matches!(first, Ack::Accepted { .. }), "{first:?}");
    assert_eq!(
        built.session.send(set()).await,
        Ack::Duplicate {
            original: Box::new(first)
        }
    );
    // 被拒绝的命令重发，回的是原来的拒绝。
    let take = || {
        envelope(
            "c2",
            Command::Dequeue {
                command: CommandId::new("nothing"),
            },
        )
    };
    let rejected = built.session.send(take()).await;
    assert!(matches!(rejected, Ack::Rejected { .. }), "{rejected:?}");
    assert_eq!(
        built.session.send(take()).await,
        Ack::Duplicate {
            original: Box::new(rejected)
        }
    );
}

/// 以前同 id、不同内容的命令也回 `Duplicate`，后一条被静默吞掉。
#[tokio::test]
async fn the_same_id_with_another_command_is_a_conflict() {
    let built = Setup::new(Arc::new(ScriptedModel::new(Vec::new())))
        .build()
        .await;
    let first = built
        .session
        .send(envelope("c1", Command::SetPlan { enabled: true }))
        .await;
    assert!(matches!(first, Ack::Accepted { .. }), "{first:?}");
    let again = built
        .session
        .send(envelope("c1", Command::SetPlan { enabled: false }))
        .await;
    assert!(matches!(again, Ack::Rejected { .. }), "{again:?}");
}

/// 起名的模型：收到请求时通知 `entered`，等 `gate` 放行才回复（回复读不出名字，起名回 `Rejected`）。
struct EnteredModel {
    entered: Arc<Notify>,
    inner: GatedModel,
}

impl ModelPort for EnteredModel {
    fn stream(
        &self,
        request: rust_cli_kernel_api::model::ModelRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>,
    > {
        self.entered.notify_one();
        self.inner.stream(request, cancel)
    }
}

/// 原命令还在等结果（不带参数的 `/rename` 等起名的回复）时到达的重发，等它出结果，回的是同一个结果。
#[tokio::test]
async fn a_repeat_of_a_pending_command_waits_for_its_result() {
    let main = Arc::new(ScriptedModel::new(vec![text_response(&["I will look."])]));
    let (entered, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let mut setup = Setup::new(main);
    setup.side_model = Some(Arc::new(EnteredModel {
        entered: entered.clone(),
        inner: GatedModel::new(gate.clone()),
    }));
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c0", "Fix the login test")).await;
    until(&mut events, finished).await;
    let session = Arc::new(built.session);
    let first = tokio::spawn({
        let session = session.clone();
        async move { session.send(envelope("n1", Command::GenerateName)).await }
    });
    entered.notified().await;
    // 重发先进 mailbox（`join!` 先轮询它，发送在第一次轮询里完成），`view` 排在它之后：`view` 回来时重发已经处理过。
    let (again, ()) = tokio::join!(session.send(envelope("n1", Command::GenerateName)), async {
        session.view().await.unwrap();
        gate.notify_one();
    });
    let first = first.await.unwrap();
    assert!(
        !matches!(
            first,
            Ack::Accepted {
                result: CommandResult::Done
            }
        ),
        "{first:?}"
    );
    assert_eq!(
        again,
        Ack::Duplicate {
            original: Box::new(first)
        }
    );
}
