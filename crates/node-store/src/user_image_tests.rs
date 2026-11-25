//! 人类输入里的图片（docs/specs/rust-cli/tui-input.md §3.2）：写成 Node `file` part（data URL），冷读取按 part
//! 顺序还原成 image 块。
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::Value;

use crate::tests::{TestStore, batch, started};

#[tokio::test]
async fn pasted_images_round_trip_as_node_file_parts() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let image = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        origin: None,
    };
    let message = Message::User(UserMessage {
        id: MessageId::new("msg_ru_u1"),
        content: vec![
            ContentBlock::Text {
                text: "what is this [Image #1]".into(),
            },
            image,
        ],
        origin: UserOrigin::Human,
    });
    let fact = Fact::MessageAppended {
        message: message.clone(),
    };
    t.store
        .commit(&SessionId::new("sess_1"), batch(Some("cmd"), vec![fact]))
        .await
        .unwrap();
    let parts = t.rows("select data from part where message_id = 'msg_ru_u1' order by sequence");
    let file: Value = parts
        .iter()
        .filter_map(|row| row[0].as_str())
        .map(|data| serde_json::from_str::<Value>(data).unwrap())
        .find(|part| part["type"] == "file")
        .expect("a file part");
    assert_eq!(file["mime"], "image/png");
    assert_eq!(file["url"], "data:image/png;base64,iVBORw0KGgo=");
    assert_eq!(file["metadata"]["storageKind"], "inline");
    let loaded = t.store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(loaded.messages, vec![message]);
    // 标题只取文字。
    assert_eq!(loaded.meta.title, "what is this [Image #1]");
}

/// 带内容块的 attachment（tui-input.md §1.3、node-store.md §4.5）：内容块写成同一条合成消息里 text part 之后的
/// part（文字块是 synthetic text part，图片是 Node `file` part），`rustCli.data` 不重复存；冷读取按 part 顺序还原。
#[tokio::test]
async fn attachment_blocks_round_trip_as_parts_after_the_text() {
    use rust_cli_kernel_api::message::{ATTACHMENT_BLOCKS, AttachmentMessage};
    use serde_json::json;

    let t = TestStore::new();
    started(&t, "sess_1").await;
    let image = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        origin: None,
    };
    let cell = ContentBlock::Text {
        text: "<system-reminder>\n<cell id=\"p\">show()</cell id=\"p\">\n</system-reminder>".into(),
    };
    let human = Message::User(UserMessage {
        id: MessageId::new("msg_ru_u1"),
        content: vec![ContentBlock::Text {
            text: "look at @plot.ipynb".into(),
        }],
        origin: UserOrigin::Human,
    });
    let attachment = Message::Attachment(AttachmentMessage {
        id: MessageId::new("msg_rv_a1"),
        source: "file".into(),
        text: "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/w/plot.ipynb\"}\n</system-reminder>".into(),
        data: json!({
            "filename": "/w/plot.ipynb",
            "displayPath": "plot.ipynb",
            ATTACHMENT_BLOCKS: [cell.clone(), image.clone()],
        }),
    });
    let facts = [human.clone(), attachment.clone()]
        .into_iter()
        .map(|message| Fact::MessageAppended { message })
        .collect();
    t.store
        .commit(&SessionId::new("sess_1"), batch(Some("cmd"), facts))
        .await
        .unwrap();
    let parts: Vec<Value> = t
        .rows("select data from part where message_id = 'msg_rv_a1' order by sequence")
        .iter()
        .filter_map(|row| row[0].as_str())
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    let kinds: Vec<&str> = parts
        .iter()
        .filter_map(|part| part["type"].as_str())
        .collect();
    assert_eq!(kinds, ["text", "text", "file"]);
    assert_eq!(parts[0]["metadata"]["rustCli"]["message"], "attachment");
    assert_eq!(
        parts[0]["metadata"]["rustCli"]["data"],
        json!({"filename": "/w/plot.ipynb", "displayPath": "plot.ipynb"})
    );
    assert_eq!(parts[1]["synthetic"], true);
    assert_eq!(parts[1]["metadata"]["source"], "file");
    assert_eq!(parts[2]["url"], "data:image/png;base64,iVBORw0KGgo=");
    let loaded = t.store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(loaded.messages, vec![human, attachment]);
}

/// 只有附件、没有正文的输入（session-lifecycle.md §10.14）：存成一条真实输入，带一个空的 text part 与标记；冷读还原成没有
/// 内容块的人类消息。它的输入记录（`rust-cli/input`）与它同一批，冷读读回。没有标记的空消息照旧去掉。
#[tokio::test]
async fn an_input_without_text_round_trips_with_its_record() {
    use rust_cli_kernel_api::input_extras::{AttachedFile, InputRecord};

    let t = TestStore::new();
    started(&t, "sess_1").await;
    let message = Message::User(UserMessage {
        id: MessageId::new("msg_ru_u1"),
        content: Vec::new(),
        origin: UserOrigin::Human,
    });
    let record = InputRecord {
        files: vec![AttachedFile {
            path: "/Users/dev/notes.md".into(),
            name: "notes.md".into(),
            media_type: "text/markdown".into(),
            size: 3,
        }],
        ..InputRecord::new(MessageId::new("msg_ru_u1"))
    };
    let facts = vec![
        Fact::MessageAppended {
            message: message.clone(),
        },
        record.fact(),
    ];
    t.store
        .commit(&SessionId::new("sess_1"), batch(Some("cmd"), facts))
        .await
        .unwrap();
    let info = t.rows("select data from message where id = 'msg_ru_u1'");
    let info: Value = serde_json::from_str(info[0][0].as_str().unwrap()).unwrap();
    assert_eq!(info["semantics"]["origin"], "real_user");
    let loaded = t.store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(loaded.messages, vec![message]);
    assert_eq!(loaded.inputs, vec![record]);
}

/// 上传的图片（node-store.md §4.4.1）：Artifact 是 Node 的 data URL 文字（`attachment-<id>.txt`），用户消息里的图片是
/// Node 的 artifact file part（url 是 URI、`storageKind: artifact`、`sizeBytes` 是文件大小）；冷读还原成同一个 Artifact 图片块，输入记录的
/// `uploads` 读回。
#[tokio::test]
async fn an_uploaded_image_is_a_node_artifact_file_part() {
    use rust_cli_kernel_api::ids::ArtifactId;
    use rust_cli_kernel_api::input_extras::{InputRecord, UploadedFile};
    use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactStore};

    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let data_url = "data:image/png;base64,iVBORw0KGgo=";
    let artifact = t
        .store
        .put(
            &session,
            ArtifactPurpose::Attachment,
            "text/plain".into(),
            data_url.as_bytes().to_vec(),
        )
        .await
        .unwrap();
    // Node 按 artifact id 找文件、按扩展名读文字：文件名带 id、是 .txt
    let names = rust_cli_fs::file_names(&t.store.paths().artifacts().as_path().join("sess_1"))
        .await
        .unwrap();
    assert_eq!(names, [format!("attachment-{}.txt", artifact.id)]);
    assert_eq!(
        t.store.find(&session, &artifact.id).await.unwrap().as_ref(),
        Some(&artifact)
    );
    let message = Message::User(UserMessage {
        id: MessageId::new("msg_ru_u1"),
        content: vec![
            ContentBlock::Text {
                text: "look".into(),
            },
            ContentBlock::Image {
                source: ImageSource::Artifact {
                    media_type: "image/png".into(),
                    artifact: artifact.clone(),
                },
                origin: None,
            },
        ],
        origin: UserOrigin::Human,
    });
    let record = InputRecord {
        uploads: vec![UploadedFile {
            session: session.clone(),
            artifact: ArtifactId::new(artifact.id.as_str()),
            name: "shot.png".into(),
            media_type: "image/png".into(),
            size: 8,
        }],
        ..InputRecord::new(MessageId::new("msg_ru_u1"))
    };
    let facts = vec![
        Fact::MessageAppended {
            message: message.clone(),
        },
        record.fact(),
    ];
    t.store
        .commit(&session, batch(Some("cmd"), facts))
        .await
        .unwrap();
    let file: Value = t
        .rows("select data from part where message_id = 'msg_ru_u1' order by sequence")
        .iter()
        .filter_map(|row| row[0].as_str())
        .map(|data| serde_json::from_str::<Value>(data).unwrap())
        .find(|part| part["type"] == "file")
        .expect("a file part");
    let uri = format!("zcode-artifact://sess_1/{}", artifact.id);
    assert_eq!(file["mime"], "image/png");
    assert_eq!(file["url"], uri.as_str());
    assert_eq!(file["metadata"]["storageKind"], "artifact");
    assert_eq!(file["metadata"]["artifactUri"], uri.as_str());
    assert_eq!(file["metadata"]["sizeBytes"], data_url.len());
    let loaded = t.store.load(&session).await.unwrap();
    assert_eq!(loaded.messages, vec![message]);
    assert_eq!(loaded.inputs, vec![record]);
}

/// ATTACH-2（node-store.md §4.4.2）：用户附上的 PDF（user 内容里的 document）写成文字 part 之后的 Node artifact `file` part，
/// `mime` 是 `application/pdf`（Node 冷恢复的 `isPdfMime` 一支读回 `file` 块）；冷读按 `rustCli.artifact` 还原同一个块，排在图片之前
/// 的顺序不变。
#[tokio::test]
async fn an_attached_pdf_round_trips_as_a_node_artifact_file_part() {
    use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactStore};

    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let artifact = t
        .store
        .put(
            &session,
            ArtifactPurpose::Attachment,
            "text/plain".into(),
            b"data:application/pdf;base64,JVBERi0=".to_vec(),
        )
        .await
        .unwrap();
    let document = ContentBlock::Document {
        source: ImageSource::Artifact {
            media_type: "application/pdf".into(),
            artifact: artifact.clone(),
        },
        origin: None,
    };
    let image = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        origin: None,
    };
    let message = Message::User(UserMessage {
        id: MessageId::new("msg_ru_u1"),
        content: vec![
            ContentBlock::Text {
                text: "read it".into(),
            },
            document,
            image,
        ],
        origin: UserOrigin::Human,
    });
    t.store
        .commit(
            &session,
            batch(
                Some("cmd"),
                vec![Fact::MessageAppended {
                    message: message.clone(),
                }],
            ),
        )
        .await
        .unwrap();
    let files: Vec<Value> = t
        .rows("select data from part where message_id = 'msg_ru_u1' order by sequence")
        .iter()
        .filter_map(|row| row[0].as_str())
        .map(|data| serde_json::from_str::<Value>(data).unwrap())
        .filter(|part| part["type"] == "file")
        .collect();
    let uri = format!("zcode-artifact://sess_1/{}", artifact.id);
    assert_eq!(files[0]["mime"], "application/pdf");
    assert_eq!(files[0]["url"], uri.as_str());
    assert_eq!(files[0]["metadata"]["storageKind"], "artifact");
    assert_eq!(files[0]["metadata"]["artifactUri"], uri.as_str());
    assert_eq!(files[1]["mime"], "image/png");
    let loaded = t.store.load(&session).await.unwrap();
    assert_eq!(loaded.messages, vec![message]);
}

/// SMALL-4（node-store.md §4.4.2）：块的 origin 照 Node 的 file part 写——本机附上的写 `filename`、`source`（路径、界面给的原文与它的
/// UTF-16 长度）、`metadata.originalUrl`；上传的 PDF 只写 `filename`；上传的图片不写。冷读还原同样的 origin。
#[tokio::test]
async fn attached_media_keep_their_file_like_node() {
    use rust_cli_kernel_api::message::MediaOrigin;
    use serde_json::json;

    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let png = || ImageSource::Base64 {
        media_type: "image/png".into(),
        data: "iVBORw0KGgo=".into(),
    };
    let origin = |file_name: &str, path: Option<&str>| {
        Some(MediaOrigin {
            file_name: Some(file_name.into()),
            path: path.map(str::to_owned),
        })
    };
    let message = Message::User(UserMessage {
        id: MessageId::new("msg_ru_u1"),
        content: vec![
            ContentBlock::Text {
                text: "look".into(),
            },
            ContentBlock::Image {
                source: png(),
                origin: origin("图.png", Some("/Users/dev/图.png")),
            },
            ContentBlock::Document {
                source: ImageSource::Base64 {
                    media_type: "application/pdf".into(),
                    data: "JVBERi0=".into(),
                },
                origin: origin("doc.pdf", None),
            },
            ContentBlock::Image {
                source: png(),
                origin: origin("shot.png", None),
            },
        ],
        origin: UserOrigin::Human,
    });
    t.store
        .commit(
            &session,
            batch(
                Some("cmd"),
                vec![Fact::MessageAppended {
                    message: message.clone(),
                }],
            ),
        )
        .await
        .unwrap();
    let files: Vec<Value> = t
        .rows("select data from part where message_id = 'msg_ru_u1' order by sequence")
        .iter()
        .filter_map(|row| row[0].as_str())
        .map(|data| serde_json::from_str::<Value>(data).unwrap())
        .filter(|part| part["type"] == "file")
        .collect();
    assert_eq!(files[0]["filename"], "图.png");
    assert_eq!(
        files[0]["source"],
        json!({"type": "file", "path": "/Users/dev/图.png", "text": {"value": "/Users/dev/图.png", "start": 0, "end": 16}})
    );
    assert_eq!(files[0]["metadata"]["originalUrl"], "/Users/dev/图.png");
    assert_eq!(files[1]["filename"], "doc.pdf");
    assert!(files[1].get("source").is_none());
    assert!(files[2].get("filename").is_none() && files[2].get("source").is_none());
    let loaded = t.store.load(&session).await.unwrap();
    let Message::User(read) = &loaded.messages[0] else {
        panic!("a user message");
    };
    let origins: Vec<Option<MediaOrigin>> = read
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { origin, .. } | ContentBlock::Document { origin, .. } => {
                Some(origin.clone())
            }
            _ => None,
        })
        .collect();
    // 上传的图片的名字不落库（Node 相同），冷读之后没有
    assert_eq!(
        origins,
        [
            origin("图.png", Some("/Users/dev/图.png")),
            origin("doc.pdf", None),
            None
        ]
    );
}
