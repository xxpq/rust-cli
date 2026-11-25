//! 附上的媒体与上传的文件（docs/specs/rust-cli/session-lifecycle.md §10.40、§10.41）：开回合时经 `UserInputs::media` 解析，块排在
//! 用户消息的正文之后，上传的文字随这条输入提交；媒体文件不经 `attached`；显示的事实进输入记录；排队时带着它们。解析本身（缩放、
//! 类型、占位）在 tools 的 `attachment_media` 测。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::input_extras::{
    AttachedFile, InputExtras, PROMPT_ATTACHMENT_SOURCE, UploadedFile,
};
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::session_api::{Command, Delivery, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};

use crate::input_tests::FakeInputs;
use crate::test_support::{Setup, envelope, finished, until};

fn upload(name: &str, media_type: &str) -> UploadedFile {
    UploadedFile {
        session: SessionId::new("sess_1"),
        artifact: ArtifactId::new("tool-result-1"),
        name: name.to_owned(),
        media_type: media_type.to_owned(),
        size: 8,
    }
}

fn file(path: &str, media_type: &str) -> AttachedFile {
    AttachedFile {
        path: path.to_owned(),
        name: "shown".to_owned(),
        media_type: media_type.to_owned(),
        size: 1,
    }
}

fn send(
    id: &str,
    text: &str,
    extras: InputExtras,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::Submit {
            text: text.to_owned(),
            extras,
            delivery: Delivery::Queue,
            target: None,
        },
    )
}

fn uploads(uploads: Vec<UploadedFile>) -> InputExtras {
    InputExtras {
        uploads,
        ..InputExtras::default()
    }
}

fn said(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
    }
}

/// 跑一条输入，交回视图。
async fn run(
    setup: Setup,
    command: rust_cli_kernel_api::session_api::CommandEnvelope,
) -> rust_cli_kernel_api::session_api::SessionView {
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(command).await;
    until(&mut events, finished).await;
    built.session.view().await.unwrap()
}

fn setup(model: Arc<ScriptedModel>) -> Setup {
    let mut setup = Setup::new(model);
    setup.inputs = Arc::new(FakeInputs);
    setup
}

fn user_content(view: &rust_cli_kernel_api::session_api::SessionView) -> Vec<ContentBlock> {
    let Some(Message::User(user)) = view.messages.first() else {
        panic!("the input expected: {:?}", view.messages);
    };
    user.content.clone()
}

#[tokio::test]
async fn media_come_after_the_prompt_text() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let shot = upload("shot.png", "image/png");
    let view = run(
        setup(model),
        send("c1", "look", uploads(vec![shot.clone()])),
    )
    .await;
    assert_eq!(
        user_content(&view),
        vec![said("look"), said("media shot.png")]
    );
    assert_eq!(view.inputs[0].uploads, vec![shot]);
}

/// 只有上传的文件、正文为空：用户消息只有它的块。
#[tokio::test]
async fn an_input_of_only_an_upload_is_its_block() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let view = run(
        setup(model),
        send("c1", "", uploads(vec![upload("shot.png", "image/png")])),
    )
    .await;
    assert_eq!(user_content(&view), vec![said("media shot.png")]);
}

/// 上传的文字是随这条输入提交的 attachment（排在用户消息之后），模型收到它。
#[tokio::test]
async fn an_uploaded_text_is_committed_with_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let view = run(
        setup(model.clone()),
        send("c1", "", uploads(vec![upload("notes.txt", "text/plain")])),
    )
    .await;
    assert!(
        matches!(&view.messages[1], Message::Attachment(attachment) if attachment.source == PROMPT_ATTACHMENT_SOURCE),
        "{:?}",
        view.messages
    );
    let request = serde_json::to_string(&model.requests()[0].messages).unwrap();
    assert!(
        request.contains("Attached inline text: notes.txt"),
        "{request}"
    );
}

/// 本机的媒体文件经 `media`（用户消息里），别的本机文件照旧经 `attached`（attachment）。
#[tokio::test]
async fn media_files_go_to_media_and_other_files_to_attached() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let extras = InputExtras {
        files: vec![
            file("/w/pic.png", "image/png"),
            file("/w/a.txt", "text/plain"),
        ],
        ..InputExtras::default()
    };
    let view = run(setup(model), send("c1", "look", extras)).await;
    assert_eq!(
        user_content(&view),
        vec![said("look"), said("media /w/pic.png")]
    );
    let attached: Vec<&str> = view
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Attachment(attachment) => Some(attachment.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        attached,
        ["<system-reminder>\nattached /w/a.txt\n</system-reminder>"]
    );
}

/// 运行中带上传文件的输入排队（自己一轮），排队项带着它；轮到它时照样是它的块。
#[tokio::test]
async fn a_queued_upload_opens_its_own_turn_with_its_block() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["second"]),
    ]));
    let shot = upload("shot.png", "image/png");
    let built = setup(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(envelope(
            "c1",
            Command::Bash {
                command: "slow".to_owned(),
                delivery: Delivery::Queue,
            },
        ))
        .await;
    until(&mut events, |event| {
        matches!(
            event,
            rust_cli_kernel_api::session_api::KernelEvent::TurnStarted { .. }
        )
    })
    .await;
    built
        .session
        .send(send("c2", "queued", uploads(vec![shot.clone()])))
        .await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue.len(), 1);
    assert_eq!(view.queue[0].extras.uploads, vec![shot]);
    built
        .session
        .send(envelope("i1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let with_media = view
        .messages
        .iter()
        .filter(|message| {
            matches!(message, Message::User(user) if user.content.contains(&said("media shot.png")))
        })
        .count();
    assert_eq!(with_media, 1);
    let request = serde_json::to_string(&model.requests().last().unwrap().messages).unwrap();
    assert!(request.contains("media shot.png"), "{request}");
}
