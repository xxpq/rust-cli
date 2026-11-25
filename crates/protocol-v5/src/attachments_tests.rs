//! attachments@1（protocol-v5.md §39.3、§39.4、§39.6）：四条命令的规则表、提交在任务里（链路不等）、shutdown 等在途的提交、
//! 没有行时读面不授权。带行的授权与读回字节由真进程的一致性场景测（`session-input-upload`：进程内的内核替身没有会话）。
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, ArtifactStore, StoreError};
use rust_cli_kernel_api::testing::{MemoryArtifacts, MemoryStore};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::{Harness, initialize};

#[cfg(not(windows))]
const KEY: &str = "/Users/dev/repo";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\repo";
const SESSION: &str = "ses_up";
/// 1×1 的 PNG（与 E2E 粘贴的同一张）。
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACklEQVR4AWMAAQAABQABNtCI3QAAAABJRU5ErkJggg==";

fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG)
        .unwrap()
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 第 n 个命令 id（UUIDv7 的形状）。
fn id(n: u64) -> String {
    format!("019a5f3c-7a2b-7c3d-8e4f-{n:012}")
}

struct Local;

#[async_trait]
impl Workspaces for Local {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        Ok(crate::test_link::kernel())
    }

    fn release(&self, _key: &str) -> bool {
        false
    }

    fn workspace(&self, key: &str) -> rust_cli_kernel_api::env::WorkspaceRef {
        rust_cli_kernel_api::env::WorkspaceRef {
            environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
            root: rust_cli_kernel_api::env::EnvPath::new(key),
        }
    }

    async fn mcp_pending(&self, _key: &str) -> Vec<crate::mcp_trust::McpPending> {
        Vec::new()
    }

    async fn trust_mcp(
        &self,
        _key: &str,
        _decision: crate::mcp_trust::McpDecision,
    ) -> Result<crate::mcp_trust::McpTrusted, crate::mcp_trust::McpTrustRefusal> {
        unreachable!("no trust in these tests")
    }
}

/// 写入要等闸门的 Artifact 存储（替身里的「磁盘很慢」）；读照常。
#[derive(Default)]
struct Gated {
    inner: MemoryArtifacts,
    gate: Option<Notify>,
}

#[async_trait]
impl ArtifactStore for Gated {
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.inner.put(session, purpose, media_type, bytes).await
    }

    async fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        self.inner.get(artifact).await
    }

    async fn find(
        &self,
        session: &SessionId,
        id: &ArtifactId,
    ) -> Result<Option<ArtifactRef>, StoreError> {
        self.inner.find(session, id).await
    }
}

/// 握手、报进程级部件建好（Artifact 存储是 `artifacts`）。
async fn ready(artifacts: Arc<dyn ArtifactStore>) -> Harness {
    let mut h = Harness::new();
    h.handle
        .ready(Arc::new(Local), Arc::new(MemoryStore::default()), artifacts);
    h.send(initialize()).await;
    h.next().await;
    h
}

fn command(rpc: u64, command: u64, kind: &str, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": rpc, "method": "command", "params": {
        "commandId": id(command), "type": kind,
        "target": {"hostId": "local", "workspaceKey": KEY, "sessionId": SESSION},
        "payload": payload,
    }})
}

/// 发一条命令，回它的结果（JSON-RPC 的 `result`，或整条错误回复）。
async fn send(h: &mut Harness, rpc: u64, n: u64, kind: &str, payload: Value) -> Value {
    h.send(command(rpc, n, kind, payload)).await;
    let reply = h.reply(rpc, &mut Vec::new()).await;
    reply.get("result").cloned().unwrap_or(reply)
}

/// 契约的单个上传上限（`ATTACHMENT_LIMITS.maxBytes`）。
const MAX_BYTES: usize = 20 * 1024 * 1024;

fn begin(size: usize) -> Value {
    json!({"name": "shot.png", "mime": "image/png", "size": size})
}

fn chunk(upload: &str, offset: usize, bytes: &[u8]) -> Value {
    json!({"uploadId": upload, "offset": offset, "bytes": encode(bytes)})
}

fn commit(upload: &str, bytes: &[u8]) -> Value {
    json!({"uploadId": upload, "sha256": format!("sha256:{}", rust_cli_base::hash::sha256_hex(bytes))})
}

fn code(result: &Value) -> &str {
    result["code"].as_str().unwrap_or_default()
}

/// begin → 两块 → commit：uploadId 是 begin 的命令 id，ref 是会话的 Artifact URI，Artifact 是认出的格式的 data URL；重放 commit 与
/// begin 回原结果。
#[tokio::test]
async fn an_upload_becomes_a_data_url_artifact() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let mut h = ready(artifacts.clone()).await;
    let bytes = png();
    let started = send(&mut h, 2, 1, "attachment.begin", begin(bytes.len())).await;
    assert_eq!(started["kind"], "accepted", "{started}");
    let upload = started["result"]["uploadId"].as_str().unwrap().to_owned();
    assert_eq!(upload, id(1));
    let (head, tail) = bytes.split_at(20);
    assert_eq!(
        send(&mut h, 3, 2, "attachment.chunk", chunk(&upload, 0, head)).await["kind"],
        "accepted"
    );
    assert_eq!(
        send(&mut h, 4, 3, "attachment.chunk", chunk(&upload, 20, tail)).await["kind"],
        "accepted"
    );
    let done = send(&mut h, 5, 4, "attachment.commit", commit(&upload, &bytes)).await;
    assert_eq!(done["kind"], "accepted", "{done}");
    let reference = done["result"]["ref"].as_str().unwrap().to_owned();
    assert_eq!(done["result"]["attachmentId"], upload.as_str());
    let (session, artifact) = rust_cli_kernel_api::artifact_uri::parse(&reference).unwrap();
    assert_eq!(session.as_str(), SESSION);
    let stored = artifacts.find(&session, &artifact).await.unwrap().unwrap();
    assert_eq!(stored.purpose, ArtifactPurpose::Attachment);
    assert_eq!(stored.media_type, "text/plain");
    assert_eq!(
        String::from_utf8(artifacts.get(&stored).await.unwrap()).unwrap(),
        format!("data:image/png;base64,{PNG}")
    );
    // 重放：commit 回同一个 ref；begin（同一个命令 id、同一份元数据）回同一个 uploadId
    let again = send(&mut h, 6, 5, "attachment.commit", commit(&upload, &bytes)).await;
    assert_eq!(again["result"]["ref"], reference.as_str());
    let replayed = send(&mut h, 7, 1, "attachment.begin", begin(bytes.len())).await;
    assert_eq!(replayed["result"]["uploadId"], upload.as_str());
    let conflict = send(&mut h, 8, 1, "attachment.begin", begin(bytes.len() + 1)).await;
    assert_eq!(
        code(&conflict),
        "precondition.commandIdConflict",
        "{conflict}"
    );
    assert_eq!(artifacts.count(), 1);
}

/// chunk 按偏移（§39.3）：同一段重传 noop；错位 → chunkGap；同一位置不同字节 → chunkConflict；超出声明的大小 → sizeExceeded；
/// 不是 base64 → invalidPayload；别的会话里没有这个上传 → notFound。
#[tokio::test]
async fn chunks_follow_the_byte_offset() {
    let mut h = ready(Arc::new(MemoryArtifacts::default())).await;
    let bytes = png();
    let upload =
        send(&mut h, 2, 1, "attachment.begin", begin(bytes.len())).await["result"]["uploadId"]
            .as_str()
            .unwrap()
            .to_owned();
    let (head, tail) = bytes.split_at(20);
    assert_eq!(
        send(&mut h, 3, 2, "attachment.chunk", chunk(&upload, 0, head)).await["kind"],
        "accepted"
    );
    let resent = send(&mut h, 4, 3, "attachment.chunk", chunk(&upload, 0, head)).await;
    assert_eq!(
        (resent["kind"].clone(), resent["noop"].clone()),
        (json!("accepted"), json!(true)),
        "{resent}"
    );
    let gap = send(&mut h, 5, 4, "attachment.chunk", chunk(&upload, 30, tail)).await;
    assert_eq!(code(&gap), "precondition.chunkGap", "{gap}");
    let conflict = send(
        &mut h,
        6,
        5,
        "attachment.chunk",
        chunk(&upload, 0, &[0; 20]),
    )
    .await;
    assert_eq!(code(&conflict), "precondition.chunkConflict", "{conflict}");
    let over = send(&mut h, 7, 6, "attachment.chunk", chunk(&upload, 20, &bytes)).await;
    assert_eq!(code(&over), "precondition.sizeExceeded", "{over}");
    let garbage = send(
        &mut h,
        8,
        7,
        "attachment.chunk",
        json!({"uploadId": upload, "offset": 20, "bytes": "not base64!"}),
    )
    .await;
    assert_eq!(
        (code(&garbage), garbage["field"].clone()),
        ("precondition.invalidPayload", json!("bytes")),
        "{garbage}"
    );
    let mut elsewhere = command(9, 8, "attachment.chunk", chunk(&upload, 20, tail));
    elsewhere["params"]["target"]["sessionId"] = json!("ses_other");
    h.send(elsewhere).await;
    let other = h.reply(9, &mut Vec::new()).await;
    assert_eq!(code(&other["result"]), "precondition.notFound", "{other}");
}

/// begin 的门（§39.3、§41.5）：大小的上限是契约的 20 MiB（schema 守，超了 → invalidPayload），3.75 MB 以上的图片照收（开回合时缩放）；
/// 名字带换行 → invalidPayload；同时进行中的上传到 16 个 → tooManyUploads；没有 sessionId → invalidPayload；命令 id 不是 UUIDv7 →
/// invalidCommandId。
#[tokio::test]
async fn begin_admits_what_an_upload_can_be() {
    let mut h = ready(Arc::new(MemoryArtifacts::default())).await;
    let large = send(&mut h, 2, 1, "attachment.begin", begin(MAX_BYTES + 1)).await;
    assert_eq!(
        (code(&large), large["field"].clone()),
        ("precondition.invalidPayload", json!("size")),
        "{large}"
    );
    let screenshot = send(
        &mut h,
        4,
        3,
        "attachment.begin",
        begin(rust_cli_kernel_api::image::MAX_IMAGE_BYTES + 1),
    )
    .await;
    assert_eq!(screenshot["kind"], "accepted", "{screenshot}");
    send(&mut h, 5, 4, "attachment.abort", json!({"uploadId": id(3)})).await;
    let named = send(
        &mut h,
        3,
        2,
        "attachment.begin",
        json!({"name": "a\nb.png", "mime": "image/png", "size": 1}),
    )
    .await;
    assert_eq!(
        (code(&named), named["field"].clone()),
        ("precondition.invalidPayload", json!("name")),
        "{named}"
    );
    for n in 0..16 {
        let started = send(&mut h, 10 + n, 10 + n, "attachment.begin", begin(1)).await;
        assert_eq!(started["kind"], "accepted", "{started}");
    }
    let many = send(&mut h, 40, 40, "attachment.begin", begin(1)).await;
    assert_eq!(code(&many), "precondition.tooManyUploads", "{many}");
    let mut no_session = command(41, 41, "attachment.begin", begin(1));
    no_session["params"]["target"]
        .as_object_mut()
        .unwrap()
        .remove("sessionId");
    h.send(no_session).await;
    let missing = h.reply(41, &mut Vec::new()).await;
    assert_eq!(
        code(&missing["result"]),
        "precondition.invalidPayload",
        "{missing}"
    );
    let mut bad_id = command(42, 42, "attachment.begin", begin(1));
    bad_id["params"]["commandId"] = json!("not-a-uuid");
    h.send(bad_id).await;
    let invalid = h.reply(42, &mut Vec::new()).await;
    assert_eq!(
        code(&invalid["result"]),
        "precondition.invalidCommandId",
        "{invalid}"
    );
}

/// 存储还没就绪时不开始上传（暂存没有地方提交）。
#[tokio::test]
async fn begin_waits_for_the_store() {
    let mut h = Harness::new();
    h.send(initialize()).await;
    h.next().await;
    let early = send(&mut h, 2, 1, "attachment.begin", begin(1)).await;
    assert_eq!(code(&early), "precondition.storeUnavailable", "{early}");
}

/// 提交失败删掉暂存（§39.3）：sha256 不符 → checksumMismatch，之后 commit → notFound；没收全 → uploadIncomplete（暂存留着，补齐之后
/// 能提交）。
#[tokio::test]
async fn a_failed_commit_drops_the_upload() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let mut h = ready(artifacts.clone()).await;
    let bytes = png();
    let upload = id(1);
    send(&mut h, 2, 1, "attachment.begin", begin(bytes.len())).await;
    let (head, tail) = bytes.split_at(20);
    send(&mut h, 3, 2, "attachment.chunk", chunk(&upload, 0, head)).await;
    let short = send(&mut h, 4, 3, "attachment.commit", commit(&upload, &bytes)).await;
    assert_eq!(code(&short), "precondition.uploadIncomplete", "{short}");
    send(&mut h, 5, 4, "attachment.chunk", chunk(&upload, 20, tail)).await;
    let wrong = send(&mut h, 6, 5, "attachment.commit", commit(&upload, b"other")).await;
    assert_eq!(code(&wrong), "precondition.checksumMismatch", "{wrong}");
    let gone = send(&mut h, 7, 6, "attachment.commit", commit(&upload, &bytes)).await;
    assert_eq!(code(&gone), "precondition.notFound", "{gone}");
    assert_eq!(artifacts.count(), 0);
}

/// 提交收任何类型（§41.5，Node `writePromptAttachment`）：认得出的图片取文件头的格式，别的取声明的类型（带参数照写）；解不开的「图片」
/// 也照收（开回合时换成占位）。
#[tokio::test]
async fn a_commit_keeps_any_type() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let mut h = ready(artifacts.clone()).await;
    let cases: [(&str, Vec<u8>, String); 3] = [
        (
            "text/plain;charset=utf-8",
            b"plain text".to_vec(),
            format!(
                "data:text/plain;charset=utf-8;base64,{}",
                encode(b"plain text")
            ),
        ),
        (
            "image/png",
            vec![0; 16],
            format!("data:image/png;base64,{}", encode(&[0; 16])),
        ),
        // 声明的类型与字节不符：取认出的格式（同 §39.3）
        ("image/jpeg", png(), format!("data:image/png;base64,{PNG}")),
    ];
    for (n, (mime, bytes, expected)) in cases.into_iter().enumerate() {
        let n = n as u64 * 10;
        let upload = id(n + 1);
        let begun = json!({"name": "a", "mime": mime, "size": bytes.len()});
        send(&mut h, n + 2, n + 1, "attachment.begin", begun).await;
        send(
            &mut h,
            n + 3,
            n + 2,
            "attachment.chunk",
            chunk(&upload, 0, &bytes),
        )
        .await;
        let done = send(
            &mut h,
            n + 4,
            n + 3,
            "attachment.commit",
            commit(&upload, &bytes),
        )
        .await;
        assert_eq!(done["kind"], "accepted", "{mime}: {done}");
        let (session, artifact) =
            rust_cli_kernel_api::artifact_uri::parse(done["result"]["ref"].as_str().unwrap())
                .unwrap();
        let stored = artifacts.find(&session, &artifact).await.unwrap().unwrap();
        assert_eq!(
            String::from_utf8(artifacts.get(&stored).await.unwrap()).unwrap(),
            expected,
            "{mime}"
        );
    }
}

/// 暂存的总预算（§41.5，Node 在 chunk 时守）：全部上传已收的字节加这一块超过 64 MiB → stagingCapacityExceeded；abort 之后放出名额。
#[tokio::test]
async fn chunks_keep_within_the_staging_budget() {
    let mut h = ready(Arc::new(MemoryArtifacts::default())).await;
    let block = vec![0; crate::consts::CHUNK_MAX_BYTES];
    let per_upload = MAX_BYTES / crate::consts::CHUNK_MAX_BYTES;
    let mut rpc = 100;
    // 三个 20 MiB 加第四个的 4 MiB：正好 64 MiB
    for (n, chunks) in [(1, per_upload), (2, per_upload), (3, per_upload), (4, 8)] {
        send(
            &mut h,
            n,
            id_number(n),
            "attachment.begin",
            begin(MAX_BYTES),
        )
        .await;
        for index in 0..chunks {
            rpc += 1;
            let accepted = send(
                &mut h,
                rpc,
                rpc,
                "attachment.chunk",
                chunk(&id(id_number(n)), index * block.len(), &block),
            )
            .await;
            assert_eq!(accepted["kind"], "accepted", "{n}/{index}: {accepted}");
        }
    }
    let next = chunk(&id(id_number(4)), 8 * block.len(), &block);
    let over = send(&mut h, 1000, 1000, "attachment.chunk", next.clone()).await;
    assert_eq!(
        code(&over),
        "precondition.stagingCapacityExceeded",
        "{over}"
    );
    send(
        &mut h,
        1001,
        1001,
        "attachment.abort",
        json!({"uploadId": id(id_number(1))}),
    )
    .await;
    let after = send(&mut h, 1002, 1002, "attachment.chunk", next).await;
    assert_eq!(after["kind"], "accepted", "{after}");
}

/// 预算测试里第 n 个上传的命令 id 编号（与 chunk 的编号分开）。
fn id_number(n: u64) -> u64 {
    5000 + n
}

/// 提交在任务里（§39.2、§4.1）：写盘卡住时 `ping` 先回；提交中的 chunk 与 abort → commitInProgress；同一条 commit 重放等同一次
/// 提交的结果；shutdown 的回复排在提交的 ACK 之后。
#[tokio::test]
async fn a_commit_runs_in_a_task_and_the_link_keeps_going() {
    let store = Arc::new(Gated {
        inner: MemoryArtifacts::default(),
        gate: Some(Notify::new()),
    });
    let mut h = ready(store.clone()).await;
    let bytes = png();
    let upload = id(1);
    send(&mut h, 2, 1, "attachment.begin", begin(bytes.len())).await;
    send(&mut h, 3, 2, "attachment.chunk", chunk(&upload, 0, &bytes)).await;
    h.send(command(4, 3, "attachment.commit", commit(&upload, &bytes)))
        .await;
    h.send(json!({"jsonrpc": "2.0", "id": 5, "method": "ping", "params": {}}))
        .await;
    assert_eq!(h.next().await["id"], 5);
    let late = send(&mut h, 6, 4, "attachment.chunk", chunk(&upload, 0, &bytes)).await;
    assert_eq!(code(&late), "precondition.commitInProgress", "{late}");
    let abort = send(
        &mut h,
        7,
        5,
        "attachment.abort",
        json!({"uploadId": upload}),
    )
    .await;
    assert_eq!(code(&abort), "precondition.commitInProgress", "{abort}");
    h.send(command(8, 3, "attachment.commit", commit(&upload, &bytes)))
        .await;
    h.send(json!({"jsonrpc": "2.0", "id": 9, "method": "shutdown", "params": {}}))
        .await;
    store.gate.as_ref().unwrap().notify_one();
    let first = h.next().await;
    let second = h.next().await;
    let third = h.next().await;
    assert_eq!(
        (
            first["id"].clone(),
            second["id"].clone(),
            third["id"].clone()
        ),
        (json!(4), json!(8), json!(9))
    );
    assert_eq!(first["result"]["kind"], "accepted", "{first}");
    assert_eq!(first["result"]["result"], second["result"]["result"]);
}

/// abort（§39.3）：暂存中 → 删掉，之后的 chunk → notFound；没有这个上传 → noop。
#[tokio::test]
async fn abort_drops_staging_and_is_a_noop_otherwise() {
    let mut h = ready(Arc::new(MemoryArtifacts::default())).await;
    let upload = id(1);
    send(&mut h, 2, 1, "attachment.begin", begin(4)).await;
    let aborted = send(
        &mut h,
        3,
        2,
        "attachment.abort",
        json!({"uploadId": upload}),
    )
    .await;
    assert_eq!(aborted["kind"], "accepted", "{aborted}");
    let after = send(&mut h, 4, 3, "attachment.chunk", chunk(&upload, 0, b"abcd")).await;
    assert_eq!(code(&after), "precondition.notFound", "{after}");
    let none = send(
        &mut h,
        5,
        4,
        "attachment.abort",
        json!({"uploadId": upload}),
    )
    .await;
    assert_eq!(none["noop"], true, "{none}");
}

/// 读面先授权（§39.4）：这个会话的投影里没有带这个 ref 的用户行 → notAuthorized；`previewSource` 同一条授权。
#[tokio::test]
async fn reads_need_a_row_that_carries_the_ref() {
    let mut h = ready(Arc::new(MemoryArtifacts::default())).await;
    for (rpc, kind) in [
        (2, "attachment.read"),
        (3, "attachment.stat"),
        (4, "attachment.previewSource"),
    ] {
        h.send(
            json!({"jsonrpc": "2.0", "id": rpc, "method": "query", "params": {
                "type": kind,
                "target": {"hostId": "local", "workspaceKey": KEY, "sessionId": SESSION},
                "params": {"ref": format!("zcode-artifact://{SESSION}/tool-result-1")},
            }}),
        )
        .await;
        let reply = h.reply(rpc, &mut Vec::new()).await;
        assert_eq!(
            reply["error"]["data"]["code"], "precondition.notAuthorized",
            "{kind}: {reply}"
        );
    }
}
