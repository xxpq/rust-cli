//! 工具调用与结果 ↔ Node tool part（spec node-store.md §4.6）。
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::ids::{ArtifactId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, ImageSource, Message, StopReason, ToolResultContent,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, Fact, SessionStore, StoreError};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, started, text, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn call(id: &str, name: &str, input: Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: name.into(),
        input,
    }
}

fn calls(id: &str, content: Vec<ContentBlock>) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content,
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    }
}

fn result(id: &str, owner: &str, call: &str, content: Vec<ToolResultContent>, error: bool) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(call),
                content,
                is_error: error,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(owner),
            },
        }),
    }
}

fn said(text: &str) -> ToolResultContent {
    ToolResultContent::Text { text: text.into() }
}

fn messages(facts: &[Fact]) -> Vec<Message> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended { message } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

async fn commit_each(t: &TestStore, facts: &[Fact]) {
    for fact in facts {
        t.store
            .commit(&sid(), batch(None, vec![fact.clone()]))
            .await
            .unwrap();
    }
}

/// tool_use 与结果合成同一个 tool part，结果不新建消息行；冷读取拆回内核的消息形状，结果按提交
/// 顺序（并发调用可能后声明的先结束）。位置键是结算时最后一行（这里都是 assistant）。
#[tokio::test]
async fn tool_calls_and_results_read_back_as_kernel_messages() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let thinking = ContentBlock::Thinking {
        thinking: "look".into(),
        signature: Some("sig".into()),
    };
    let facts = vec![
        user("msg_ru_u1", "Read two files"),
        calls(
            "msg_rv_a1",
            vec![
                thinking,
                text("Reading."),
                call("toolu_1", "Read", json!({"file_path": "/w/a"})),
                call("toolu_2", "Read", json!({"file_path": "/w/b"})),
            ],
        ),
        result("msg_rw_r2", "msg_rv_a1", "toolu_2", vec![said("b")], false),
        result(
            "msg_rx_r1",
            "msg_rv_a1",
            "toolu_1",
            vec![said("no such file")],
            true,
        ),
        assistant("msg_s0_a2", vec![text("Done.")], None),
    ];
    commit_each(&t, &facts).await;
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages, messages(&facts));

    let rows = t.rows("select id from message where id in ('msg_rw_r2', 'msg_rx_r1')");
    assert!(rows.is_empty(), "{rows:?}");
    let parts = t.rows(
        "select data from part where message_id = 'msg_rv_a1' and data like '{\"type\":\"tool\"%'
         order by sequence",
    );
    let completed = json!({"type": "tool", "callID": "toolu_2", "declarationIndex": 1,
        "tool": "Read", "metadata": {"rustCli": {"resultMessageId": "msg_rw_r2", "resultIndex": 0,
            "afterMessageId": "msg_rv_a1"}},
        "state": {"status": "completed", "input": {"file_path": "/w/b"}, "output": "b",
            "title": "Read", "metadata": {"schemaVersion": 1},
            "time": {"start": 1003, "end": 1004}}});
    let failed = json!({"type": "tool", "callID": "toolu_1", "declarationIndex": 0,
        "tool": "Read", "metadata": {"rustCli": {"resultMessageId": "msg_rx_r1", "resultIndex": 1,
            "afterMessageId": "msg_rv_a1"}},
        "state": {"status": "error", "input": {"file_path": "/w/a"}, "error": "no such file",
            "metadata": {"modelContent": "no such file"},
            "time": {"start": 1003, "end": 1005}}});
    assert_eq!(
        parts,
        vec![
            vec![Value::from(stringify(&failed))],
            vec![Value::from(stringify(&completed))]
        ]
    );
}

/// 结果里的图片照 Node 的媒体规则存：`state.attachments` 里的 file part 加 `modelContentLayout`。
/// 内联图片是 data URL，Artifact 引用是 `zcode-artifact://`。
#[tokio::test]
async fn images_and_artifacts_in_results_round_trip() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let artifact = ArtifactRef {
        session: sid(),
        id: ArtifactId::new("tool-result-1"),
        purpose: ArtifactPurpose::Media,
        media_type: "image/png".into(),
        size: 3,
    };
    let content = vec![
        said("two images"),
        ToolResultContent::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".into(),
                data: "iVBO".into(),
            },
        },
        ToolResultContent::Image {
            source: ImageSource::Artifact {
                media_type: "image/png".into(),
                artifact,
            },
        },
    ];
    let facts = vec![
        user("msg_ru_u1", "Show"),
        calls(
            "msg_rv_a1",
            vec![call("toolu_1", "Read", json!({"file_path": "/w/i.png"}))],
        ),
        result("msg_rw_r1", "msg_rv_a1", "toolu_1", content, false),
        calls(
            "msg_rx_a2",
            vec![call("toolu_2", "Glob", json!({"pattern": "*"}))],
        ),
        result("msg_ry_r2", "msg_rx_a2", "toolu_2", vec![], false),
    ];
    commit_each(&t, &facts).await;
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages, messages(&facts));

    let state = t.rows(
        "select json_extract(data, '$.state.output'), json_extract(data, '$.state.metadata'),
           json_extract(data, '$.state.attachments[0].url'),
           json_extract(data, '$.state.attachments[1].url')
         from part where message_id = 'msg_rv_a1' and json_extract(data, '$.type') = 'tool'",
    );
    let layout = json!({"schemaVersion": 1, "modelContentLayout": [
        {"type": "text", "text": "two images"},
        {"type": "attachment", "attachmentIndex": 0},
        {"type": "attachment", "attachmentIndex": 1}]});
    assert_eq!(
        state,
        vec![vec![
            Value::from("two images\n\n[Attached image/png]\n\n[Attached image/png]"),
            Value::from(stringify(&layout)),
            Value::from("data:image/png;base64,iVBO"),
            Value::from("zcode-artifact://sess_1/tool-result-1"),
        ]]
    );
}

/// Read 整读 PDF 的 document 块（node-store.md §4.6，修复线 F24）：与图片同一写法的 file part（`mime` 是
/// `application/pdf`，data URL），冷读取还原成 document 块（修前读回 `[Attached application/pdf: …]` 文字）。
#[tokio::test]
async fn pdf_documents_in_results_round_trip() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let content = vec![
        said("PDF file read: /w/doc.pdf (5 bytes)"),
        ToolResultContent::Document {
            source: ImageSource::Base64 {
                media_type: "application/pdf".into(),
                data: "JVBERi0=".into(),
            },
        },
    ];
    let facts = vec![
        user("msg_ru_u1", "Read it"),
        calls(
            "msg_rv_a1",
            vec![call("toolu_1", "Read", json!({"file_path": "/w/doc.pdf"}))],
        ),
        result("msg_rw_r1", "msg_rv_a1", "toolu_1", content, false),
    ];
    commit_each(&t, &facts).await;
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages, messages(&facts));
    let state = t.rows(
        "select json_extract(data, '$.state.output'),
           json_extract(data, '$.state.attachments[0].mime'),
           json_extract(data, '$.state.attachments[0].url'),
           json_extract(data, '$.state.attachments[0].metadata.storageKind')
         from part where message_id = 'msg_rv_a1' and json_extract(data, '$.type') = 'tool'",
    );
    assert_eq!(
        state,
        vec![vec![
            Value::from("PDF file read: /w/doc.pdf (5 bytes)\n\n[Attached application/pdf]"),
            Value::from("application/pdf"),
            Value::from("data:application/pdf;base64,JVBERi0="),
            Value::from("inline"),
        ]]
    );
}

/// 中断：被拒的调用写成 error，中断提示是合成消息；没有结果的调用读回时只有 tool_use。
#[tokio::test]
async fn interrupted_and_unfinished_calls_keep_their_shape() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let reject = "The user doesn't want to proceed with this tool use.";
    let facts = vec![
        user("msg_ru_u1", "Go"),
        calls(
            "msg_rv_a1",
            vec![
                call("toolu_1", "Bash", json!({"command": "sleep 9"})),
                call("toolu_2", "", json!({})),
            ],
        ),
        result(
            "msg_rw_r1",
            "msg_rv_a1",
            "toolu_1",
            vec![said(reject)],
            true,
        ),
        Fact::MessageAppended {
            message: Message::User(UserMessage {
                id: MessageId::new("msg_rx_i1"),
                content: vec![text("[Request interrupted by user for tool use]")],
                origin: UserOrigin::Human,
            }),
        },
    ];
    commit_each(&t, &facts).await;
    let loaded = t.store.load(&sid()).await.unwrap();
    // toolu_2 没有结果：assistant 里仍有它的 tool_use，后面只有 toolu_1 的结果与中断提示。
    assert_eq!(loaded.messages, messages(&facts));
    let empty = t.rows(
        "select json_extract(data, '$.tool'), json_extract(data, '$.metadata.providerToolName'),
           json_extract(data, '$.state.status')
         from part where json_extract(data, '$.callID') = 'toolu_2'",
    );
    assert_eq!(
        empty,
        vec![vec![
            Value::from("empty_tool_name"),
            Value::from(""),
            Value::from("pending")
        ]]
    );
}

#[tokio::test]
async fn results_need_their_pending_call() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let facts = vec![
        user("msg_ru_u1", "Go"),
        calls("msg_rv_a1", vec![call("toolu_1", "Read", json!({}))]),
        result("msg_rw_r1", "msg_rv_a1", "toolu_1", vec![said("x")], false),
    ];
    commit_each(&t, &facts).await;
    let refused = [
        result(
            "msg_rx_r2",
            "msg_rv_a1",
            "toolu_1",
            vec![said("again")],
            false,
        ),
        result("msg_ry_r3", "msg_rv_a1", "toolu_9", vec![said("x")], false),
        result("msg_rz_r4", "msg_none", "toolu_1", vec![said("x")], false),
    ];
    for fact in refused {
        let outcome = t.store.commit(&sid(), batch(None, vec![fact])).await;
        assert!(
            matches!(outcome, Err(StoreError::Storage { .. })),
            "{outcome:?}"
        );
    }
    let mixed = Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new("msg_s0_r5"),
            content: vec![text("not a result")],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("msg_rv_a1"),
            },
        }),
    };
    let outcome = t.store.commit(&sid(), batch(None, vec![mixed])).await;
    assert!(
        matches!(outcome, Err(StoreError::Unsupported { .. })),
        "{outcome:?}"
    );
}

/// ToolSearch 的 `tool_reference`：`output` 是降级文本（Node 续写可读），layout 记引用，本工程按 layout
/// 读回（docs/specs/rust-cli/tools-more.md §3.6）。
#[tokio::test]
async fn tool_references_round_trip_with_a_text_fallback() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let reference = |name: &str| ToolResultContent::ToolReference {
        tool_name: name.into(),
    };
    let facts = vec![
        user("msg_tu_u1", "Search"),
        calls(
            "msg_tv_a1",
            vec![call(
                "toolu_1",
                "ToolSearch",
                json!({"query": "select:WebFetch,WebSearch"}),
            )],
        ),
        result(
            "msg_tw_r1",
            "msg_tv_a1",
            "toolu_1",
            vec![reference("WebFetch"), reference("WebSearch")],
            false,
        ),
    ];
    commit_each(&t, &facts).await;
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages, messages(&facts));

    let state = t.rows(
        "select json_extract(data, '$.state.output'), json_extract(data, '$.state.metadata')
         from part where message_id = 'msg_tv_a1' and json_extract(data, '$.type') = 'tool'",
    );
    let layout = json!({"schemaVersion": 1, "modelContentLayout": [
        {"type": "tool_reference", "tool_name": "WebFetch"},
        {"type": "tool_reference", "tool_name": "WebSearch"}]});
    assert_eq!(
        state,
        vec![vec![
            Value::from("Tool loaded: WebFetch\n\nTool loaded: WebSearch"),
            Value::from(stringify(&layout)),
        ]]
    );
}

/// 修复线 F12：`time.start` 取同批 `ToolCalled` 的开工时刻（Node `settleToolResults` 写 `result.startedAt`）；
/// 同批没有计量时才取 assistant 的创建时间。以前一律取 assistant 的创建时间，与 Node 不同
/// （`records_tools_tests.rs` 的逐字节比对发现）。
#[tokio::test]
async fn the_tool_start_is_the_executor_start_from_the_same_batch() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let facts = vec![
        user("msg_ru_u1", "Go"),
        calls(
            "msg_rv_a1",
            vec![
                call("toolu_1", "Read", json!({"file_path": "/w/a"})),
                call("toolu_2", "Read", json!({"file_path": "/w/b"})),
            ],
        ),
    ];
    commit_each(&t, &facts).await;
    let metered = rust_cli_kernel_api::metering::ToolCall {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Read".into(),
        started_at_ms: 1004,
        completed_at_ms: 1005,
        status: rust_cli_kernel_api::metering::CallStatus::Completed,
        output_bytes: 1,
    };
    let first = result("msg_rx_r1", "msg_rv_a1", "toolu_1", vec![said("a")], false);
    let batch_with_call = vec![first, Fact::ToolCalled { call: metered }];
    t.store
        .commit(&sid(), batch(None, batch_with_call))
        .await
        .unwrap();
    let second = result("msg_ry_r2", "msg_rv_a1", "toolu_2", vec![said("b")], false);
    t.store
        .commit(&sid(), batch(None, vec![second]))
        .await
        .unwrap();
    let times = t.rows(
        "select json_extract(data, '$.state.time') from part
         where json_extract(data, '$.type') = 'tool' order by sequence",
    );
    assert_eq!(
        times,
        vec![
            vec![Value::from(r#"{"start":1004,"end":1005}"#)],
            vec![Value::from(r#"{"start":1003,"end":1006}"#)],
        ]
    );
}

/// 修复线 F12：执行器的业务失败给模型的是 `<tool_use_error>原文</tool_use_error>`。Node `createErrorResult`
/// 把原文放在 `state.error`（App 工具卡显示它），带标签的模型文本放在 `metadata.modelContent`。以前
/// `state.error` 也存带标签的文本，App 显示出标签。冷读取读 `modelContent`，模型文本不变。
#[tokio::test]
async fn a_tool_use_error_keeps_the_bare_message_for_the_app() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let envelope = "<tool_use_error>File does not exist.</tool_use_error>";
    let facts = vec![
        user("msg_ru_u1", "Go"),
        calls(
            "msg_rv_a1",
            vec![call("toolu_1", "Read", json!({"file_path": "/w/a"}))],
        ),
        result(
            "msg_rw_r1",
            "msg_rv_a1",
            "toolu_1",
            vec![said(envelope)],
            true,
        ),
    ];
    commit_each(&t, &facts).await;
    let state = t.rows(
        "select json_extract(data, '$.state.error'), json_extract(data, '$.state.metadata')
         from part where json_extract(data, '$.type') = 'tool'",
    );
    let metadata = json!({"modelContent": envelope});
    assert_eq!(
        state,
        vec![vec![
            Value::from("File does not exist."),
            Value::from(stringify(&metadata)),
        ]]
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages, messages(&facts));
}

fn output_head(call: &str, head: &str) -> rust_cli_kernel_api::command_output::ToolOutputHead {
    rust_cli_kernel_api::command_output::ToolOutputHead {
        tool_use: ToolUseId::new(call),
        head: rust_cli_kernel_api::command_output::CommandOutputHead {
            head: head.into(),
            output_file: format!("/tmp/zcode-1000/sess_1/tasks/{call}.output"),
        },
    }
}

/// 调用的输出头部（PROJ-GAPS，node-store.md §4.6 第 7 条）：与结果同批、排在结果之后，写进 completed 与 error 的 part 的
/// `state.metadata`（Node `completedToolPartMetadata` 的 display 与它要的 `schemaVersion`）；冷读取读回 `output_heads`，消息不变。
/// 以前不写，冷读没有头部。Node 写的 display 在同一个键，同样读回；不是 `bash_output` 的不读。
#[tokio::test]
async fn output_heads_are_stored_in_the_part_metadata_and_read_back() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let facts = vec![
        user("msg_ru_u1", "Print a lot"),
        calls(
            "msg_rv_a1",
            vec![
                call("toolu_ok", "Bash", json!({"command": "seq 100000"})),
                call(
                    "toolu_fail",
                    "Bash",
                    json!({"command": "seq 100000; exit 3"}),
                ),
                call("toolu_node", "Bash", json!({"command": "seq 100000"})),
            ],
        ),
    ];
    commit_each(&t, &facts).await;
    let ok = result(
        "msg_rw_r1",
        "msg_rv_a1",
        "toolu_ok",
        vec![said("<persisted-output>")],
        false,
    );
    let fail = result(
        "msg_rx_r2",
        "msg_rv_a1",
        "toolu_fail",
        vec![said("Exit code 3\n1")],
        true,
    );
    let node = result(
        "msg_ry_r3",
        "msg_rv_a1",
        "toolu_node",
        vec![said("<persisted-output>")],
        false,
    );
    for (fact, head) in [
        (ok.clone(), Some(output_head("toolu_ok", "1\n2"))),
        (fail.clone(), Some(output_head("toolu_fail", "1"))),
        (node.clone(), None),
    ] {
        let mut batch_facts = vec![fact];
        batch_facts.extend(head.map(|head| Fact::OutputHead { head }));
        t.store
            .commit(&sid(), batch(None, batch_facts))
            .await
            .unwrap();
    }
    let display = |call: &str, head: &str| {
        json!({"kind": "bash_output", "output": head, "truncated": true,
            "outputPath": format!("/tmp/zcode-1000/sess_1/tasks/{call}.output")})
    };
    let metadata = t.rows(
        "select json_extract(data, '$.callID'), json_extract(data, '$.state.metadata')
         from part where json_extract(data, '$.type') = 'tool' order by json_extract(data, '$.declarationIndex')",
    );
    assert_eq!(
        metadata,
        vec![
            vec![
                Value::from("toolu_ok"),
                Value::from(stringify(
                    &json!({"schemaVersion": 1, "display": display("toolu_ok", "1\n2")})
                )),
            ],
            vec![
                Value::from("toolu_fail"),
                Value::from(stringify(
                    &json!({"modelContent": "Exit code 3\n1", "schemaVersion": 1,
                    "display": display("toolu_fail", "1")})
                )),
            ],
            vec![
                Value::from("toolu_node"),
                Value::from(stringify(&json!({"schemaVersion": 1}))),
            ],
        ]
    );
    // Node 写的 display（同一个键、Node 的形状）与别的种类
    let conn = t.connection();
    conn.execute(
        "update part set data = json_set(data, '$.state.metadata.display', json(?))
         where json_extract(data, '$.callID') = 'toolu_node'",
        [stringify(&display("toolu_node", "from node"))],
    )
    .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    let mut all = facts.clone();
    all.extend([ok, fail, node]);
    assert_eq!(loaded.messages, messages(&all));
    assert_eq!(
        loaded.output_heads,
        vec![
            output_head("toolu_ok", "1\n2"),
            output_head("toolu_fail", "1"),
            output_head("toolu_node", "from node"),
        ]
    );
    conn.execute(
        "update part set data = json_set(data, '$.state.metadata.display', json('{\"kind\":\"file_diff\"}'))
         where json_extract(data, '$.callID') = 'toolu_node'",
        [],
    )
    .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.output_heads.len(), 2);
}

/// TOOL-TIME（session-lifecycle.md §10.37）：`time` 的起止都取同批计量的 `ToolCall::time`（Node 的 `end` 是 `result.completedAt`）。以前
/// `time.end` 是结果消息的创建时间（这里是 1005、1006），与内核给前端的结束不是同一个值。同批没有计量时照旧（开始是 assistant 的创建时间 1003，
/// 结束是结果消息的创建时间 1007）。冷读的 `tool_times` 按存储顺序读回 completed 与 error 的 part（Node 写的同一个键），pending 的不给。
#[tokio::test]
async fn the_tool_time_is_the_metered_start_and_end_and_reads_back() {
    use rust_cli_kernel_api::metering::{CallStatus, ToolCall, ToolTime};

    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let read = |id: &str| call(id, "Read", json!({"file_path": format!("/w/{id}")}));
    let facts = vec![
        user("msg_ru_u1", "Go"),
        calls(
            "msg_rv_a1",
            vec![
                read("toolu_ok"),
                read("toolu_fail"),
                read("toolu_node"),
                read("toolu_open"),
            ],
        ),
    ];
    commit_each(&t, &facts).await;
    let metered = |call: &str, started_at_ms, completed_at_ms| Fact::ToolCalled {
        call: ToolCall {
            tool_use: ToolUseId::new(call),
            name: "Read".into(),
            started_at_ms,
            completed_at_ms,
            status: CallStatus::Completed,
            output_bytes: 1,
        },
    };
    let ok = result("msg_rx_r1", "msg_rv_a1", "toolu_ok", vec![said("a")], false);
    let fail = result(
        "msg_ry_r2",
        "msg_rv_a1",
        "toolu_fail",
        vec![said("no such file")],
        true,
    );
    let node = result(
        "msg_rz_r3",
        "msg_rv_a1",
        "toolu_node",
        vec![said("c")],
        false,
    );
    for facts in [
        vec![ok, metered("toolu_ok", 1004, 1010)],
        vec![fail, metered("toolu_fail", 1004, 1020)],
        vec![node],
    ] {
        t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    }
    let times = t.rows(
        "select json_extract(data, '$.state.time') from part
         where json_extract(data, '$.type') = 'tool' order by json_extract(data, '$.declarationIndex')",
    );
    assert_eq!(
        times,
        vec![
            vec![Value::from(r#"{"start":1004,"end":1010}"#)],
            vec![Value::from(r#"{"start":1004,"end":1020}"#)],
            vec![Value::from(r#"{"start":1003,"end":1007}"#)],
            vec![Value::Null],
        ]
    );
    // Node 写的起止（同一个键）
    t.connection()
        .execute(
            "update part set data = json_set(data, '$.state.time', json('{\"start\":1100,\"end\":1150}'))
             where json_extract(data, '$.callID') = 'toolu_node'",
            [],
        )
        .unwrap();
    let time = |call: &str, started_at, ended_at| ToolTime {
        tool_use: ToolUseId::new(call),
        started_at,
        ended_at,
    };
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        loaded.tool_times,
        vec![
            time("toolu_ok", 1004, 1010),
            time("toolu_fail", 1004, 1020),
            time("toolu_node", 1100, 1150),
        ]
    );
}
