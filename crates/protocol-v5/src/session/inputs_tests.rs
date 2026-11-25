//! `input.send` 的附件（protocol-v5.md §15.4.1、§39.5）：本机绝对路径的成为附件，Artifact URI 的成为上传的文件，别的接受后丢弃（C1）；
//! 行与命令同一个形状。

use rust_cli_base::models::GLM_5_3_ACCOUNT;
use serde_json::json;

use super::*;

#[cfg(not(windows))]
const FILE: &str = "/Users/dev/a/notes.md";
#[cfg(windows)]
const FILE: &str = r"C:\Users\dev\a\notes.md";

fn send(attachments: serde_json::Value) -> CommandInputSend {
    serde_json::from_value(json!({"text": "see", "attachments": attachments})).unwrap()
}

/// ATTACHMENTS 改（§39.5）：以前 Artifact URI 也丢弃（rust 不服务 attachments@1），现在它是上传的文件。
#[test]
fn local_files_and_uploads_become_attachments_and_others_are_dropped() {
    let local = json!({"ref": FILE, "fileName": "notes.md", "mime": "text/markdown", "bytes": 12,
        "sourceKind": "clipboard-text", "messageCount": 2});
    let uploaded = json!({"ref": "zcode-artifact://ses_1/a1", "fileName": "shot.png",
        "mime": "image/png", "bytes": 3});
    let malformed = json!({"ref": FILE});
    let elsewhere = json!({"ref": "https://example.com/shot.png", "fileName": "shot.png",
        "mime": "image/png", "bytes": 3});
    let extras = extras(&send(json!([uploaded, local, malformed, elsewhere]))).unwrap();
    assert_eq!(
        extras.files,
        [AttachedFile {
            path: FILE.to_owned(),
            name: "notes.md".to_owned(),
            media_type: "text/markdown".to_owned(),
            size: 12,
        }]
    );
    assert_eq!(
        extras.uploads,
        [UploadedFile {
            session: rust_cli_kernel_api::ids::SessionId::new("ses_1"),
            artifact: rust_cli_kernel_api::ids::ArtifactId::new("a1"),
            name: "shot.png".to_owned(),
            media_type: "image/png".to_owned(),
            size: 3,
        }]
    );
    // 行与队列项写回命令里的形状（只有读进内核的四项）：先是本机文件，再是上传的文件，ref 是同一个 URI
    assert_eq!(
        attachments(&extras.files, &extras.uploads),
        json!([{"ref": FILE, "fileName": "notes.md", "mime": "text/markdown", "bytes": 12},
            {"ref": "zcode-artifact://ses_1/a1", "fileName": "shot.png", "mime": "image/png", "bytes": 3}])
    );
}

fn send_with(payload: serde_json::Value) -> CommandInputSend {
    serde_json::from_value(payload).unwrap()
}

/// 引用、来源、页面状态（K-P1–K-P3）进内核；行写回契约的 `InputQuote` / `InputSource` 形状。
#[test]
fn quotes_source_and_page_state_reach_the_kernel() {
    let extras = extras(&send_with(json!({
        "text": "hi",
        "quotes": [{"text": "q", "senderName": "Ada", "sentAt": 5}],
        "source": {"kind": "pluginUi", "pluginId": "p1", "toolCallId": "surface:a"},
        "genUiContext": [{"path": "/Users/dev/page.html", "state": {"step": 2}}],
    })))
    .unwrap();
    assert_eq!(extras.quotes[0].sent_at_ms, Some(5));
    assert_eq!(
        extras.relay,
        Some(Relay::Plugin {
            plugin: "p1".to_owned(),
            tool_call: "surface:a".to_owned(),
        })
    );
    assert_eq!(extras.pages[0].state, json!({"step": 2}));
    let record = extras
        .record(
            rust_cli_kernel_api::ids::MessageId::new("msg_1"),
            &Default::default(),
        )
        .unwrap();
    let mut fields = Fields::new();
    decorate(&mut fields, &record, &[]);
    assert_eq!(
        fields["quotes"],
        json!([{"text": "q", "senderName": "Ada", "sentAt": 5}])
    );
    assert_eq!(
        fields["source"],
        json!({"kind": "pluginUi", "pluginId": "p1", "toolCallId": "surface:a"})
    );
}

/// 页面状态的字节上限（每项 16 KiB、合计 64 KiB）：超出整条拒绝。
#[test]
fn page_state_over_its_byte_limit_is_refused() {
    let big = "x".repeat(GEN_UI_STATE_MAX_BYTES);
    let one = send_with(json!({
        "text": "hi",
        "genUiContext": [{"path": "/Users/dev/a.html", "state": big}],
    }));
    let Err(refused) = extras(&one) else {
        panic!("accepted");
    };
    let crate::generated::core_v1::CommandResult::Rejected(rejected) = *refused else {
        panic!("not rejected");
    };
    assert_eq!(rejected.code, codes::PRECONDITION_PAYLOAD_TOO_LARGE);
    assert_eq!(rejected.field.as_deref(), Some("genUiContext"));
    let quarter = "x".repeat(GEN_UI_STATE_MAX_BYTES - 100);
    let items: Vec<_> = (0..5)
        .map(|n| json!({"path": format!("/Users/dev/{n}.html"), "state": quarter}))
        .collect();
    assert!(extras(&send_with(json!({"text": "hi", "genUiContext": items}))).is_err());
}

/// 这一轮的执行覆盖（protocol-v5.md §38）：执行选择照 `modelSelection` 的换法；凭据引用写成契约的 JSON 文字（内核不解释，取凭据时读回同一个值）；规则原样。
#[test]
fn the_execution_override_becomes_the_turn_execution() {
    let send = send_with(json!({
        "text": "hi",
        "modelSelection": {"providerId": "account:bigmodel-team-coding-plan", "modelId": GLM_5_3_ACCOUNT},
        "modelExecution": {
            "selection": {"providerId": "account:bigmodel-highspeed-card", "modelId": GLM_5_3_ACCOUNT,
                "options": {"reasoningLevel": "high"}},
            "credential": {"kind": "highspeedCard", "cardId": "hsc_1"},
            "fallback": {
                "rules": [{"reason": "highspeed_card_expired", "providerErrorCode": "3402"},
                    {"reason": "highspeed_request_failed"}],
                "unavailable": "highspeed_selection_unavailable"
            }
        }
    }));
    let extras = extras(&send).unwrap();
    let execution = extras.execution.unwrap();
    assert_eq!(
        execution.selection,
        ModelSelection {
            reasoning: Some("high".to_owned()),
            ..ModelSelection::new("account:bigmodel-highspeed-card", GLM_5_3_ACCOUNT)
        }
    );
    let reference = execution.credential.unwrap();
    assert_eq!(
        serde_json::to_value(credential(&reference).unwrap()).unwrap(),
        json!({"kind": "highspeedCard", "cardId": "hsc_1"})
    );
    let fallback = execution.fallback.unwrap();
    assert_eq!(
        fallback.reason(Some("3402")),
        Some("highspeed_card_expired")
    );
    assert_eq!(fallback.reason(None), Some("highspeed_request_failed"));
    assert_eq!(fallback.unavailable, "highspeed_selection_unavailable");
    // 会话的模型照旧只来自 modelSelection
    assert_eq!(
        extras.config.selection.unwrap().provider,
        "account:bigmodel-team-coding-plan"
    );
    assert!(credential("not ours").is_none());
}

/// SMALL-4（§45.4）：本机附上的图片与 PDF 在这一轮写了 Artifact 时，行的这一项带 `previewRef`（按块的 origin 的路径对上），预览读它；
/// 没写的（视频、读不出的）与上传的不带。读面按 `previewRef` 也授权。
#[test]
fn local_media_point_their_preview_at_this_turns_artifact() {
    use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
    use rust_cli_kernel_api::message::{ContentBlock, ImageSource, MediaOrigin};
    use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef};

    let shot = "/Users/dev/shot.png";
    let clip = "/Users/dev/clip.mp4";
    let file = |path: &str, mime: &str| AttachedFile {
        path: path.to_owned(),
        name: path.rsplit('/').next().unwrap().to_owned(),
        media_type: mime.to_owned(),
        size: 9,
    };
    let content = [ContentBlock::Image {
        source: ImageSource::Artifact {
            media_type: "image/jpeg".to_owned(),
            artifact: ArtifactRef {
                session: SessionId::new("ses_1"),
                id: ArtifactId::new("art_1"),
                purpose: ArtifactPurpose::Attachment,
                media_type: "text/plain".to_owned(),
                size: 9,
            },
        },
        origin: Some(MediaOrigin {
            file_name: Some("shot.png".to_owned()),
            path: Some(shot.to_owned()),
        }),
    }];
    let record = InputRecord {
        files: vec![file(shot, "image/png"), file(clip, "video/mp4")],
        ..InputRecord::new(rust_cli_kernel_api::ids::MessageId::new("msg_1"))
    };
    let mut fields = Fields::new();
    decorate(&mut fields, &record, &content);
    assert_eq!(
        fields[ATTACHMENTS_KEY],
        json!([{"ref": shot, "fileName": "shot.png", "mime": "image/png", "bytes": 9,
                "previewRef": "zcode-artifact://ses_1/art_1"},
            {"ref": clip, "fileName": "clip.mp4", "mime": "video/mp4", "bytes": 9}])
    );
    let mut publisher = Publisher::new("ws/a/session/ses_1", "e1");
    let mut row = fields.clone();
    row.insert(
        super::super::rows::keys::KIND.to_owned(),
        json!(super::super::rows::kinds::USER_INPUT),
    );
    publisher.put(super::super::rows::ROWS, vec![("msg_1".to_owned(), row)]);
    assert_eq!(
        shown_attachment(
            &publisher,
            "zcode-artifact://ses_1/art_1",
            Some(("msg_1", 0))
        ),
        Some("image/png".to_owned())
    );
    assert_eq!(
        shown_attachment(&publisher, shot, Some(("msg_1", 0))),
        Some("image/png".to_owned())
    );
}

/// 抽卡时的显示事实（protocol-v5.md §46.0 第 1 条）：`highspeedMeta` 原样成这条输入的注记（键是契约的 camelCase，界面按 v4 的 schema 读）。
/// 以前接受后丢弃（§38.0 第 5 条），输入行上没有卡。没有它的输入没有注记。
#[test]
fn the_card_meta_becomes_the_input_note() {
    let meta = json!({"schemaVersion": 1, "cardId": "hsc_1", "taskId": "ses_1",
        "provider": "builtin:bigmodel-coding-plan", "model": GLM_5_3_ACCOUNT, "issuedAt": 10,
        "expiresAt": 9_000, "regularTps": 40.5});
    let send = send_with(json!({"text": "hi", "highspeedMeta": meta}));
    assert_eq!(extras(&send).unwrap().note.as_deref(), Some(&meta));
    assert_eq!(
        extras(&send_with(json!({"text": "hi"}))).unwrap().note,
        None
    );
}
