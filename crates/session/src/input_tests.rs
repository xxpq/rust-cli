//! 输入能力在会话里的接线（docs/specs/rust-cli/tui-input.md §1.1、§2.2）：普通输入的 `@` 提及在输入之后、UPS
//! 之前产生 attachment；bash 模式记两条人类消息后调模型，被中断时记 caveat、输入与中断消息。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::input_extras::{AttachedFile, InputExtras, InputRecord};
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{ToolProgress, ToolUseContext};
use rust_cli_kernel_api::user_input::{BashRun, InputAttachment, UserInputs};
use serde_json::json;
use tokio::sync::mpsc;

use rust_cli_kernel_api::session_api::{Command, Delivery, KernelEvent, TurnOutcome};

use crate::test_support::{Setup, envelope, finished, submit, until};

fn bash(id: &str, line: &str) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::Bash {
            command: line.to_owned(),
            delivery: Delivery::Queue,
        },
    )
}

/// 每个 `@名字` 给一个 `file` attachment（文字是 `<system-reminder>\nfile 名字\n</system-reminder>`）。
pub(crate) struct FakeInputs;

#[async_trait]
impl UserInputs for FakeInputs {
    async fn mentions(&self, input: &str, _context: ToolUseContext) -> Vec<InputAttachment> {
        input
            .split_whitespace()
            .filter_map(|word| word.strip_prefix('@'))
            .map(|name| InputAttachment {
                source: "file".to_owned(),
                text: format!("<system-reminder>\nfile {name}\n</system-reminder>"),
                data: json!({ "filename": name }),
            })
            .collect()
    }

    async fn attached(
        &self,
        files: &[AttachedFile],
        _context: ToolUseContext,
    ) -> Vec<InputAttachment> {
        files
            .iter()
            .filter(|file| !file.is_media())
            .map(|file| InputAttachment {
                source: "file".to_owned(),
                text: format!(
                    "<system-reminder>\nattached {}\n</system-reminder>",
                    file.path
                ),
                data: json!({ "filename": file.path }),
            })
            .collect()
    }

    /// 媒体文件与上传的文件（session-lifecycle.md §10.41）：每个一块文字（`media <路径或名字>`），上传的 `text/plain` 是一条提醒。
    async fn media(
        &self,
        files: &[AttachedFile],
        uploads: &[rust_cli_kernel_api::input_extras::UploadedFile],
        _context: ToolUseContext,
    ) -> rust_cli_kernel_api::user_input::AttachedMedia {
        let mut media = rust_cli_kernel_api::user_input::AttachedMedia::default();
        for file in files.iter().filter(|file| file.is_media()) {
            media.content.push(ContentBlock::Text {
                text: format!("media {}", file.path),
            });
        }
        for upload in uploads {
            if upload.media_type == "text/plain" {
                media
                    .attachments
                    .push(rust_cli_kernel_api::input_extras::attachment_note(
                        rust_cli_prompts::input_material::upload_text(&upload.name, "text"),
                    ));
            } else {
                media.content.push(ContentBlock::Text {
                    text: format!("media {}", upload.name),
                });
            }
        }
        media
    }

    async fn bash(
        &self,
        command: &str,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> BashRun {
        match command {
            "slow" => {
                context.cancel.cancelled().await;
                BashRun::Interrupted
            }
            "boom" => BashRun::Failed {
                message: "spawn failed".to_owned(),
            },
            other => BashRun::Completed {
                stdout: format!("ran {other}"),
                stderr: String::new(),
            },
        }
    }
}

fn kinds(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => match &user.content[..] {
                [ContentBlock::Text { text }] => format!("user:{text}"),
                other => format!("user:{other:?}"),
            },
            Message::Attachment(attachment) => format!("attachment:{}", attachment.source),
            Message::Assistant(_) => "assistant".to_owned(),
            Message::System(_) => "system".to_owned(),
        })
        .collect()
}

#[tokio::test]
async fn mentions_are_committed_after_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "look @a.txt @dir")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        [
            "user:look @a.txt @dir",
            "attachment:file",
            "attachment:file",
            "assistant"
        ]
    );
    let Message::Attachment(first) = &view.messages[1] else {
        panic!("attachment expected");
    };
    assert_eq!(first.data["filename"], "a.txt");
    // 模型收到 attachment 的文字。
    let request = format!("{:?}", model.requests()[0].messages);
    assert!(request.contains("file a.txt") && request.contains("file dir"));
}

#[tokio::test]
async fn plain_input_without_mentions_adds_nothing() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut setup = Setup::new(model);
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "mail me at a@b.c")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        ["user:mail me at a@b.c", "assistant"]
    );
}

#[tokio::test]
async fn bash_mode_records_input_and_output_then_queries() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["listed"])]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "ls")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        [
            "user:<bash-input>ls</bash-input>",
            "user:<bash-stdout>ran ls</bash-stdout><bash-stderr></bash-stderr>",
            "assistant"
        ]
    );
    let request = format!("{:?}", model.requests()[0].messages);
    assert!(
        request.contains("<bash-stdout>ran ls</bash-stdout>"),
        "{request}"
    );
    // 标题写成 `! 命令`。
    assert_eq!(view.meta.title, "! ls");
}

#[tokio::test]
async fn an_interrupted_bash_command_does_not_query() {
    let model = Arc::new(ScriptedModel::new(vec![]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "slow")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built
        .session
        .send(envelope("i1", Command::Interrupt { target: None }))
        .await;
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::TurnFinished {
            outcome: TurnOutcome::Interrupted,
            ..
        }
    )));
    let view = built.session.view().await.unwrap();
    let texts = kinds(&view.messages);
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(
        texts[0]
            .starts_with("user:<local-command-caveat>The command below was run directly in ZCode")
    );
    assert_eq!(texts[1], "user:<bash-input>slow</bash-input>");
    assert_eq!(texts[2], "user:[Request interrupted by user]");
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn bash_while_running_waits_for_the_turn_to_end() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["second"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "slow")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(bash("c2", "pwd")).await;
    let queued = until(
        &mut events,
        |event| matches!(event, KernelEvent::QueueChanged { items } if !items.is_empty()),
    )
    .await;
    let Some(KernelEvent::QueueChanged { items }) = queued.last() else {
        panic!("queue expected");
    };
    assert_eq!(items[0].text, "<bash-input>pwd</bash-input>");
    built
        .session
        .send(envelope("i1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let texts = kinds(&view.messages);
    assert!(
        texts.contains(
            &"user:<bash-stdout>ran pwd</bash-stdout><bash-stderr></bash-stderr>".to_owned()
        ),
        "{texts:?}"
    );
}

#[tokio::test]
async fn submitted_content_keeps_its_images_and_queues_while_running() {
    let image = ContentBlock::Image {
        source: rust_cli_kernel_api::message::ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        origin: None,
    };
    let content = vec![
        ContentBlock::Text {
            text: "what is [Image #1]".into(),
        },
        image.clone(),
    ];
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["a png"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "slow")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let ack = built
        .session
        .send(envelope(
            "c2",
            Command::SubmitContent {
                content: content.clone(),
                delivery: Delivery::Steer,
            },
        ))
        .await;
    assert!(matches!(
        ack,
        rust_cli_kernel_api::session_api::Ack::Accepted { .. }
    ));
    built
        .session
        .send(envelope("i1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let user = view
        .messages
        .iter()
        .find_map(|message| match message {
            Message::User(user) if user.content.len() == 2 => Some(user.content.clone()),
            _ => None,
        })
        .expect("the image input");
    assert_eq!(user, content);
    // 带图片的输入不在工具边界并入（D9）：回合结束后单独发，模型收到 image 块。
    let request = format!("{:?}", model.requests().last().unwrap().messages);
    assert!(request.contains("iVBORw0KGgo="), "{request}");
}

fn with_files(
    id: &str,
    text: &str,
    paths: &[&str],
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    let files = paths
        .iter()
        .map(|path| AttachedFile {
            path: (*path).to_owned(),
            name: "shown.md".to_owned(),
            media_type: "text/markdown".to_owned(),
            size: 3,
        })
        .collect();
    envelope(
        id,
        Command::Submit {
            text: text.to_owned(),
            extras: InputExtras {
                files,
                ..InputExtras::default()
            },
            delivery: Delivery::Queue,
            target: None,
        },
    )
}

/// 附上的文件（session-lifecycle.md §10.14）：读成 attachment，排在提及之后；输入记录与用户消息同一批落库，
/// `InputRecorded` 在这条消息的 `MessageDone` 之前。
#[tokio::test]
async fn attached_files_follow_the_mentions_and_the_record_comes_with_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(with_files("c1", "look @a.txt", &["/Users/dev/notes.md"]))
        .await;
    let seen = until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        [
            "user:look @a.txt",
            "attachment:file",
            "attachment:file",
            "assistant"
        ]
    );
    let Message::Attachment(attached) = &view.messages[2] else {
        panic!("attachment expected");
    };
    assert_eq!(attached.data["filename"], "/Users/dev/notes.md");
    let request = format!("{:?}", model.requests()[0].messages);
    assert!(
        request.contains("attached /Users/dev/notes.md"),
        "{request}"
    );
    let prompt = view.messages[0].id().clone();
    let record = InputRecord {
        files: vec![AttachedFile {
            path: "/Users/dev/notes.md".to_owned(),
            name: "shown.md".to_owned(),
            media_type: "text/markdown".to_owned(),
            size: 3,
        }],
        ..InputRecord::new(prompt.clone())
    };
    assert_eq!(view.inputs, std::slice::from_ref(&record));
    let recorded = seen
        .iter()
        .position(|event| matches!(event, KernelEvent::InputRecorded { record: r } if *r == record))
        .expect("InputRecorded");
    let done = seen
        .iter()
        .position(|event| matches!(event, KernelEvent::MessageDone { message } if *message.id() == prompt))
        .expect("MessageDone of the input");
    assert!(recorded < done);
    // 冷读：记录在库里。
    let loaded = built
        .store
        .load(&crate::test_support::meta().id)
        .await
        .unwrap();
    assert_eq!(loaded.inputs, [record]);
}

/// 只有附件的输入（§10.14）：用户消息没有内容块，模型请求里没有空的文字块，附件照常在。
#[tokio::test]
async fn an_input_of_only_files_has_no_text_block() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(with_files("c1", "  ", &["/Users/dev/notes.md"]))
        .await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let Message::User(user) = &view.messages[0] else {
        panic!("the input expected");
    };
    assert!(user.content.is_empty());
    let request = serde_json::to_string(&model.requests()[0].messages).unwrap();
    assert!(
        request.contains("attached /Users/dev/notes.md"),
        "{request}"
    );
    assert!(!request.contains(r#""text":"  ""#), "{request}");
    assert_eq!(view.inputs.len(), 1);
}

/// 引用、来源与页面状态（§10.14）：引用紧跟输入、页面状态排在附件之后；来源与引用进输入记录，页面状态不进。引用与页面状态的
/// 写法是前端层的（这里是回声），页面状态由内核包 `<system-reminder>`（context.md §15.8）。
#[tokio::test]
async fn quotes_and_page_state_frame_the_input_for_the_model() {
    use rust_cli_kernel_api::input_extras::{
        PAGE_STATE_SOURCE, PageState, QUOTE_SOURCE, Quote, Relay,
    };

    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    setup.frontend_prompt = Some(Arc::new(rust_cli_kernel_api::testing::EchoFrontend));
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let quote = Quote {
        text: "the earlier answer".to_owned(),
        path: None,
        sender_name: None,
        sender_id: None,
        message_id: None,
        sent_at_ms: None,
    };
    let relay = Relay::Page {
        path: "/Users/dev/page.html".to_owned(),
        title: None,
    };
    let extras = InputExtras {
        files: vec![AttachedFile {
            path: "/Users/dev/notes.md".to_owned(),
            name: "notes.md".to_owned(),
            media_type: "text/markdown".to_owned(),
            size: 3,
        }],
        quotes: vec![quote.clone()],
        relay: Some(relay.clone()),
        pages: vec![PageState {
            path: "/Users/dev/page.html".to_owned(),
            state: json!({"step": 2}),
        }],
        ..InputExtras::default()
    };
    let send = envelope(
        "c1",
        Command::Submit {
            text: "explain".to_owned(),
            extras,
            delivery: Delivery::Queue,
            target: None,
        },
    );
    built.session.send(send).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        [
            "user:explain".to_owned(),
            format!("attachment:{QUOTE_SOURCE}"),
            "attachment:file".to_owned(),
            format!("attachment:{PAGE_STATE_SOURCE}"),
            "assistant".to_owned()
        ]
    );
    let record = &view.inputs[0];
    assert_eq!(record.quotes, [quote]);
    assert_eq!(record.relay, Some(relay));
    // 都在同一条 user 消息里：attachment 照对照实现的规则上浮到正文之前（normalize 的 hoist），彼此保持提交的顺序；
    // 引用是 user 角色（不折进 system）。
    let request = serde_json::to_string(&model.requests()[0].messages).unwrap();
    let quoted = request.find("quoted the earlier answer").unwrap();
    let attached = request.find("attached /Users/dev/notes.md").unwrap();
    let state = request
        .find("<system-reminder>\\npages /Users/dev/page.html\\n</system-reminder>")
        .unwrap();
    let text = request.find("explain").unwrap();
    assert!(
        quoted < attached && attached < state && state < text,
        "{request}"
    );
}

/// 每轮的工具禁用名单（§10.15）：这条输入开的回合的请求带着它，回合的模型请求里没有这些工具。
#[tokio::test]
async fn the_disallowed_tools_ride_on_the_turn_they_open() {
    use rust_cli_kernel_api::testing::FakeTool;

    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut setup = Setup::new(model.clone());
    setup.tools = vec![
        Arc::new(FakeTool::new("CronCreate", true, "scheduled")),
        Arc::new(FakeTool::new("Grep", true, "found")),
    ];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let send = envelope(
        "c1",
        Command::Submit {
            text: "run the job".to_owned(),
            extras: InputExtras {
                disallowed_tools: vec!["CronCreate".to_owned()],
                ..InputExtras::default()
            },
            delivery: Delivery::Queue,
            target: None,
        },
    );
    built.session.send(send).await;
    until(&mut events, finished).await;
    let names: Vec<String> = model.requests()[0]
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert_eq!(names, ["Grep"]);
    // 名单不落库、不显示：没有输入记录。
    assert!(built.session.view().await.unwrap().inputs.is_empty());
}
