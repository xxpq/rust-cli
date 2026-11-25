use super::*;
use crate::ids::{ArtifactId, MessageId, SessionId};

fn file(path: &str) -> AttachedFile {
    AttachedFile {
        path: path.to_owned(),
        name: "notes.md".to_owned(),
        media_type: "text/markdown".to_owned(),
        size: 12,
    }
}

fn quote() -> Quote {
    Quote {
        text: "a <b> c".to_owned(),
        path: None,
        sender_name: Some("Ada".to_owned()),
        sender_id: None,
        message_id: Some("m1".to_owned()),
        sent_at_ms: Some(0),
    }
}

#[test]
fn only_inputs_with_shown_facts_have_a_record() {
    assert_eq!(
        InputExtras::default().record(MessageId::new("msg_1"), &TurnConfig::default()),
        None
    );
    let extras = InputExtras {
        files: vec![file("/Users/dev/notes.md")],
        ..InputExtras::default()
    };
    let record = extras
        .record(MessageId::new("msg_1"), &TurnConfig::default())
        .unwrap();
    assert_eq!(record.message, MessageId::new("msg_1"));
    assert_eq!(record.files, extras.files);
}

#[test]
fn a_record_reads_back_from_its_entry() {
    let record = InputRecord {
        files: vec![file("/Users/dev/notes.md")],
        quotes: vec![quote()],
        relay: Some(Relay::Page {
            path: "/Users/dev/page.html".to_owned(),
            title: Some("Report".to_owned()),
        }),
        ..InputRecord::new(MessageId::new("msg_1"))
    };
    let Fact::Entry { kind, id, data } = record.fact() else {
        panic!("an entry expected");
    };
    assert_eq!(kind, entry_kinds::INPUT);
    assert_eq!(id, "msg_1");
    assert_eq!(InputRecord::from_entry(&id, &data), Some(record));
    assert_eq!(InputRecord::from_entry("msg_1", &Value::Null), None);
}

/// 较早写的记录只有附件：其余项读成空的。
#[test]
fn an_older_record_without_quotes_still_reads() {
    let record = InputRecord::from_entry("msg_1", &serde_json::json!({"files": []})).unwrap();
    assert!(record.quotes.is_empty() && record.relay.is_none());
}

/// 页面状态只给模型：不进记录。
#[test]
fn page_state_alone_has_no_record() {
    let extras = InputExtras {
        pages: vec![PageState {
            path: "/Users/dev/page.html".to_owned(),
            state: serde_json::json!({"step": 2}),
        }],
        ..InputExtras::default()
    };
    assert!(!extras.is_empty());
    assert_eq!(
        extras.record(MessageId::new("msg_1"), &TurnConfig::default()),
        None
    );
}

/// 输入材料一律 user 角色（context.md §15.8 的槽位 A5）：文字在 prompts 的 `input_material`，这里只配来源。页面状态以前不在 user
/// 角色表里（F12）。
#[test]
fn input_materials_are_user_role() {
    for attachment in [
        quote_attachment("q".to_owned()),
        page_state_attachment("p".to_owned()),
        attachment_note("n".to_owned()),
    ] {
        let message = crate::message::AttachmentMessage {
            id: MessageId::new("msg_1"),
            source: attachment.source,
            text: attachment.text,
            data: attachment.data,
        };
        assert_eq!(message.role(), crate::message::AttachmentRole::User);
    }
}

fn chosen(model: &str) -> crate::model::ModelSelection {
    crate::model::ModelSelection::new("p", model)
}

fn execution(mode: PermissionMode, plan: bool) -> ExecutionState {
    ExecutionState { mode, plan }
}

/// 执行配置只换给了的项（session-lifecycle.md §10.25）：没给的用会话那时的值。
#[test]
fn a_turn_config_changes_only_what_it_names() {
    let current = execution(PermissionMode::Default, true);
    assert_eq!(TurnConfig::default().execution_over(current), current);
    let guarded = TurnConfig {
        mode: Some(PermissionMode::Guarded),
        ..TurnConfig::default()
    };
    assert_eq!(
        guarded.execution_over(current),
        execution(PermissionMode::Guarded, true)
    );
    let no_plan = TurnConfig {
        plan: Some(false),
        ..TurnConfig::default()
    };
    assert_eq!(
        no_plan.execution_over(current),
        execution(PermissionMode::Default, false)
    );
}

/// 同一个选择（provider、模型、推理档位）不算换：会话那一份（含账号连接）留着。
#[test]
fn the_same_choice_is_not_a_new_selection() {
    let current = crate::model::ModelSelection {
        reasoning: Some("high".to_owned()),
        ..chosen("m1")
    };
    let same = TurnConfig {
        selection: Some(Box::new(crate::model::ModelSelection {
            reasoning: Some("high".to_owned()),
            ..chosen("m1")
        })),
        ..TurnConfig::default()
    };
    assert_eq!(same.new_selection(&current), None);
    let other = TurnConfig {
        selection: Some(Box::new(chosen("m2"))),
        ..TurnConfig::default()
    };
    assert_eq!(other.new_selection(&current), Some(&chosen("m2")));
}

/// 执行配置不让输入自己一轮（App 的每条输入都带它），但仍算进 `is_empty`：序列化时不丢。
#[test]
fn a_turn_config_does_not_make_an_own_turn() {
    let extras = InputExtras {
        config: TurnConfig {
            plan: Some(true),
            ..TurnConfig::default()
        },
        ..InputExtras::default()
    };
    assert!(!extras.own_turn());
    assert!(!extras.is_empty());
    let back: InputExtras = serde_json::from_value(serde_json::to_value(&extras).unwrap()).unwrap();
    assert_eq!(back, extras);
    let files = InputExtras {
        files: vec![file("/Users/dev/notes.md")],
        ..InputExtras::default()
    };
    assert!(files.own_turn());
}

/// 带执行覆盖的输入自己一轮（session-lifecycle.md §10.39）：不成批、不插话；执行配置照旧不算（§10.25）。
#[test]
fn an_execution_makes_its_own_turn() {
    let execution = crate::turn_execution::TurnExecution {
        selection: ModelSelection::new("fast", "m"),
        credential: Some("card".to_owned()),
        fallback: None,
    };
    let extras = InputExtras {
        execution: Some(Box::new(execution)),
        ..InputExtras::default()
    };
    assert!(extras.own_turn());
    assert!(!extras.is_empty());
    let configured = InputExtras {
        config: TurnConfig {
            selection: Some(Box::new(ModelSelection::new("p", "m"))),
            ..TurnConfig::default()
        },
        ..InputExtras::default()
    };
    assert!(!configured.own_turn());
}

fn upload() -> UploadedFile {
    UploadedFile {
        session: SessionId::new("ses_1"),
        artifact: ArtifactId::new("tool-result-1"),
        name: "shot.png".to_owned(),
        media_type: "image/png".to_owned(),
        size: 70,
    }
}

/// 上传的图片是给模型的材料（session-lifecycle.md §10.40）：只有它、正文为空的输入不是空输入，它自己一轮，进输入记录。
#[test]
fn an_upload_is_material_for_the_model_and_is_recorded() {
    let extras = InputExtras {
        uploads: vec![upload()],
        ..InputExtras::default()
    };
    assert!(extras.gives_the_model_something());
    assert!(extras.own_turn());
    let record = extras
        .record(MessageId::new("msg_1"), &TurnConfig::default())
        .unwrap();
    assert_eq!(record.uploads, vec![upload()]);
    assert!(record.files.is_empty());
}

/// 输入记录带上传附件时写 `uploads`、读回同一份；不带时不写这一项（以前写的记录字节不变），旧记录读成空。
#[test]
fn a_record_keeps_its_uploads() {
    let record = InputRecord {
        files: vec![file("/Users/dev/notes.md")],
        uploads: vec![upload()],
        ..InputRecord::new(MessageId::new("msg_1"))
    };
    let Fact::Entry { id, data, .. } = record.fact() else {
        panic!("an entry expected");
    };
    assert_eq!(
        data["uploads"],
        serde_json::json!([{"session": "ses_1", "artifact": "tool-result-1", "name": "shot.png",
            "media_type": "image/png", "size": 70}])
    );
    assert_eq!(InputRecord::from_entry(&id, &data), Some(record));
    let plain = InputRecord {
        uploads: Vec::new(),
        ..InputRecord::from_entry(&id, &data).unwrap()
    };
    let Fact::Entry { data, .. } = plain.fact() else {
        panic!("an entry expected");
    };
    assert!(data.get("uploads").is_none());
    assert!(
        InputRecord::from_entry("msg_1", &serde_json::json!({"files": []}))
            .unwrap()
            .uploads
            .is_empty()
    );
}

/// 上传附件的引用是它的 Artifact URI（session-lifecycle.md §10.40）。
#[test]
fn an_upload_is_referenced_by_its_artifact_uri() {
    assert_eq!(upload().uri(), "zcode-artifact://ses_1/tool-result-1");
}

/// ATTACH-2（§10.41）：附件按前端给的类型的主类型分三种媒体，大小写与参数不论；别的不是媒体。
#[test]
fn media_kinds_follow_the_major_type() {
    for (media_type, kind) in [
        ("image/png", Some(MediaKind::Image)),
        ("IMAGE/BMP; x=y", Some(MediaKind::Image)),
        ("video/mp4", Some(MediaKind::Video)),
        ("application/pdf", Some(MediaKind::Pdf)),
        (" Application/PDF;charset=binary", Some(MediaKind::Pdf)),
        ("text/plain", None),
        ("application/octet-stream", None),
        ("", None),
    ] {
        assert_eq!(MediaKind::of(media_type), kind, "{media_type}");
    }
    let mut picture = file("/w/a.png");
    picture.media_type = "image/png".to_owned();
    assert!(picture.is_media());
    assert!(!file("/w/notes.md").is_media());
}

/// 注记（session-lifecycle.md §10.48）：带它的输入自己一轮、进输入记录；写进 entry 的 `note`、读回同一份。没有注记时不写这一项（以前写的记录字节不变），
/// 旧记录读成 None。
#[test]
fn a_note_makes_its_own_turn_and_is_recorded() {
    let note = serde_json::json!({"cardId": "hsc_1", "expiresAt": 2_000});
    let extras = InputExtras {
        note: Some(Box::new(note.clone())),
        ..InputExtras::default()
    };
    assert!(extras.own_turn());
    assert!(!extras.gives_the_model_something());
    let record = extras
        .record(MessageId::new("msg_1"), &TurnConfig::default())
        .unwrap();
    assert_eq!(record.note, Some(note.clone()));
    let Fact::Entry { id, data, .. } = record.fact() else {
        panic!("an entry expected");
    };
    assert_eq!(data["note"], note);
    assert_eq!(InputRecord::from_entry(&id, &data), Some(record));
    let plain = InputRecord {
        note: None,
        ..InputRecord::from_entry(&id, &data).unwrap()
    };
    let Fact::Entry { data, .. } = plain.fact() else {
        panic!("an entry expected");
    };
    assert!(data.get("note").is_none());
    assert_eq!(
        InputRecord::from_entry("msg_1", &serde_json::json!({"files": []}))
            .unwrap()
            .note,
        None
    );
}

/// 补注记（`Command::PatchNote`，§10.48）：浅合并，补的键后写胜，别的键不动；没有注记时不补。
#[test]
fn a_patch_merges_into_the_note() {
    let record = InputRecord {
        note: Some(serde_json::json!({"cardId": "hsc_1", "regularTps": 30})),
        ..InputRecord::from_entry("msg_1", &serde_json::json!({})).unwrap()
    };
    let patch = serde_json::json!({"regularTps": 40, "savedDurationMs": 900});
    let patched = record.patched(patch.as_object().unwrap()).unwrap();
    assert_eq!(
        patched.note,
        Some(serde_json::json!({"cardId": "hsc_1", "regularTps": 40, "savedDurationMs": 900}))
    );
    let bare = InputRecord {
        note: None,
        ..record
    };
    assert_eq!(bare.patched(patch.as_object().unwrap()), None);
}
