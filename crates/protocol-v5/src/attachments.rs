//! attachments@1（docs/specs/rust-cli/protocol-v5.md §39、§41）：上传的暂存与读面。
//!
//! ```text
//! begin ──► 登记（uploadId = 命令 id）     chunk ──► 按偏移追加（内存，守总预算）     abort ──► 删掉暂存
//! commit ──► 提交任务：sha256、put（data URL 文字，原样；图片在开回合时才缩放）──► Event::AttachmentCommitted ──► 已提交、ACK
//! read / stat ──► 授权（会话投影里的用户行）──► 读面任务：Artifact 的 find、get、解 data URL，或本机路径经入口的 `Workspaces`
//!                 读会话主 env 上的文件（§41.4）──► Event::AttachmentRead ──► 回复
//! ```
//!
//! 暂存只归 actor（内存，断开即中止：链路关闭就是进程退出）。actor 只做内存里的一步；等 IO 的在任务里，结果经 `Event` 回来（§4.1）。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rust_cli_base::proto::mime;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::input_extras::MediaKind;
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactStore};
use rust_cli_kernel_api::{artifact_uri, data_url, image};
use rust_cli_protocol_core::rpc::RpcError;
use rust_cli_protocol_core::schema::Node;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::commands::{command_schema, field_path, invalid_payload, rejected};
use crate::consts::{
    ATTACHMENT_PREVIEW_MAX_BYTES, ATTACHMENT_READ_MAX_BYTES, CHUNK_MAX_BYTES,
    UPLOAD_MAX_CONCURRENT, UPLOAD_MAX_STAGED_BYTES,
};
use crate::errors::{Fault, protocol_error, to_rpc};
use crate::generated::attachments_v1::{
    AttachmentBeginResult, AttachmentCommitResult, CommandAttachmentAbort, CommandAttachmentBegin,
    CommandAttachmentChunk, CommandAttachmentCommit, QueryAttachmentPreviewSourceParams,
    QueryAttachmentPreviewSourceResult, QueryAttachmentPreviewSourceResultChunked,
    QueryAttachmentPreviewSourceResultLocalPath, QueryAttachmentReadParams,
    QueryAttachmentReadResult, QueryAttachmentStatResult,
};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandResultAccepted, QueryEnvelope, QueryResult,
    QueryResultOk,
};
use crate::server::{Actor, AttachmentFileError, Event, Workspaces};
use crate::topics::{local_workspace, session_topic};

pub(crate) const ATTACHMENT_BEGIN: &str = "attachment.begin";
pub(crate) const ATTACHMENT_CHUNK: &str = "attachment.chunk";
pub(crate) const ATTACHMENT_COMMIT: &str = "attachment.commit";
pub(crate) const ATTACHMENT_ABORT: &str = "attachment.abort";
pub(crate) const ATTACHMENT_READ: &str = "attachment.read";
pub(crate) const ATTACHMENT_STAT: &str = "attachment.stat";
pub(crate) const ATTACHMENT_PREVIEW_SOURCE: &str = "attachment.previewSource";

const COMMANDS: [&str; 4] = [
    ATTACHMENT_BEGIN,
    ATTACHMENT_CHUNK,
    ATTACHMENT_COMMIT,
    ATTACHMENT_ABORT,
];
const QUERIES: [&str; 3] = [ATTACHMENT_READ, ATTACHMENT_STAT, ATTACHMENT_PREVIEW_SOURCE];

/// 给 rust 的 attachments@1 schema（生成物）。命令参数是 `Command.<类型>`，查询参数是 `Query.<类型>.params`。
const SCHEMA: &str = include_str!("../schema/attachments@1.json");
const COMMAND_SCHEMA_PREFIX: &str = "Command.";
const QUERY_SCHEMA_PREFIX: &str = "Query.";
const QUERY_SCHEMA_SUFFIX: &str = ".params";
/// commit 的 `sha256` 可以带的前缀（契约：裸 hex 或 `sha256:`）。
const SHA256_PREFIX: &str = "sha256:";
/// begin 的名字里不许有的字符（契约的 refine：schema 里表达不了）。
const NAME_FORBIDDEN: [char; 3] = ['\r', '\n', '\0'];
/// 拒绝时的字段名。
const NAME_FIELD: &str = "name";
const BYTES_FIELD: &str = "bytes";
/// 只有本机视频有本机播放源（Node 的 `resolvePromptAttachmentPreviewSource`）。
const VIDEO: &str = "video/";

pub(crate) fn is_attachment_command(kind: &str) -> bool {
    COMMANDS.contains(&kind)
}

pub(crate) fn is_attachment_query(kind: &str) -> bool {
    QUERIES.contains(&kind)
}

/// 编译好的 schema（每个名字编译一次）。
fn schema(name: String) -> Option<&'static Node> {
    static CELLS: OnceLock<BTreeMap<String, OnceLock<Option<Node>>>> = OnceLock::new();
    let cells = CELLS.get_or_init(|| {
        COMMANDS
            .iter()
            .map(|kind| format!("{COMMAND_SCHEMA_PREFIX}{kind}"))
            .chain(
                QUERIES
                    .iter()
                    .map(|kind| format!("{QUERY_SCHEMA_PREFIX}{kind}{QUERY_SCHEMA_SUFFIX}")),
            )
            .map(|name| (name, OnceLock::new()))
            .collect()
    });
    command_schema(cells.get(&name)?, SCHEMA, &name)
}

/// 参数严格（11 §5）：先按 schema 校验（带字段路径），再读成生成的严格类型。
fn decode<T: DeserializeOwned>(name: String, value: &Value) -> Result<T, String> {
    if let Some(schema) = schema(name)
        && let Err(error) = schema.validate(value, "")
    {
        return Err(error);
    }
    serde_json::from_value(value.clone()).map_err(|error| format!(": {error}"))
}

fn command_payload<T: DeserializeOwned>(
    kind: &str,
    payload: &Value,
) -> Result<T, Box<CommandResult>> {
    decode(format!("{COMMAND_SCHEMA_PREFIX}{kind}"), payload)
        .map_err(|error| Box::new(invalid_payload(&error)))
}

fn accepted(result: Option<Value>, noop: bool) -> CommandResult {
    CommandResult::Accepted(CommandResultAccepted {
        effects: Vec::new(),
        noop,
        result,
    })
}

fn refused(code: &str, field: Option<&str>, message: &str) -> Box<CommandResult> {
    Box::new(rejected(
        code,
        field.map(str::to_owned),
        Some(message.to_owned()),
    ))
}

/// 一个上传在哪一步。
enum State {
    /// 按偏移收到的字节。
    Staging(Vec<u8>),
    /// 提交任务在跑：等它的结果的回复（同一个 commit 的重放也在这里等，01 §17.3 的在途合并）。
    Committing(Vec<Value>),
    /// 已提交：重放回原结果。
    Committed(AttachmentCommitResult),
}

struct Upload {
    /// begin 指名的会话（Artifact 写进它的目录）；别的会话看不到这个上传。
    session: String,
    begun: CommandAttachmentBegin,
    /// begin 时的存储：提交写进它。
    store: Arc<dyn ArtifactStore>,
    state: State,
}

/// 提交没成：业务拒绝（命令的 rejected）或写失败（JSON-RPC 的 `internal.failed`）。
pub(crate) enum CommitFailure {
    Rejected { code: &'static str, message: String },
    Failed(String),
}

/// actor 里的上传（§39.2）。
#[derive(Default)]
pub(crate) struct Uploads {
    /// 进程级的 Artifact 存储：进程级部件建好时给。
    store: Option<Arc<dyn ArtifactStore>>,
    /// uploadId → 上传。
    uploads: BTreeMap<String, Upload>,
    /// 暂存与提交中的字节之和（§41.5 的总预算）：chunk 加，abort 与提交的结果回来时减。
    staged: usize,
    /// 在跑的提交与读面任务（`shutdown` 的回复等它们，§39.6）。
    pub(crate) in_flight: usize,
}

impl Uploads {
    pub(crate) fn ready(&mut self, store: Arc<dyn ArtifactStore>) {
        self.store = Some(store);
    }

    /// 进行中的上传（暂存或提交中）。
    fn active(&self) -> usize {
        self.uploads
            .values()
            .filter(|upload| !matches!(upload.state, State::Committed(_)))
            .count()
    }

    /// 这个会话里的这个上传。
    fn get(&mut self, session: &str, id: &str) -> Option<&mut Upload> {
        self.uploads
            .get_mut(id)
            .filter(|upload| upload.session == session)
    }
}

/// 提交（在任务里）：核对 sha256，写成 data URL 的 Artifact（Node `writePromptAttachment` 的形状，§39.0 第 3 条）。data URL 的类型：
/// 认得出的图片取文件头的格式（声明的类型与字节不符时 API 会拒绝，之后的每一轮都带着这张图），别的取声明的类型（§41.5）。
async fn commit_bytes(
    store: Arc<dyn ArtifactStore>,
    (session, upload): (SessionId, String),
    (bytes, declared): (Vec<u8>, String),
    sha256: String,
) -> Result<AttachmentCommitResult, CommitFailure> {
    let expected = sha256.strip_prefix(SHA256_PREFIX).unwrap_or(&sha256);
    if rust_cli_base::hash::sha256_hex(&bytes) != expected {
        return Err(CommitFailure::Rejected {
            code: codes::PRECONDITION_CHECKSUM_MISMATCH,
            message: "the bytes do not match the sha256".to_owned(),
        });
    }
    let media_type = image::media_type(&bytes).unwrap_or(&declared);
    let text = data_url::base64(media_type, &STANDARD.encode(&bytes));
    let artifact = store
        .put(
            &session,
            ArtifactPurpose::Attachment,
            mime::TEXT_PLAIN.to_owned(),
            text.into_bytes(),
        )
        .await
        .map_err(|error| CommitFailure::Failed(error.to_string()))?;
    Ok(AttachmentCommitResult {
        attachment_id: upload,
        r#ref: artifact_uri::uri(&artifact.session, &artifact.id),
    })
}

fn query_fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

/// 读出上传的附件解码之后的字节与类型（在任务里）。
async fn read_upload(
    store: Arc<dyn ArtifactStore>,
    reference: &str,
) -> Result<(Vec<u8>, String), Fault> {
    let not_found = || query_fault(codes::PRECONDITION_NOT_FOUND, "the attachment is gone");
    let (session, id) = artifact_uri::parse(reference).ok_or_else(not_found)?;
    let failed = |error: rust_cli_kernel_api::store::StoreError| {
        query_fault(codes::INTERNAL_FAILED, error.to_string())
    };
    let artifact = store
        .find(&session, &id)
        .await
        .map_err(failed)?
        .ok_or_else(not_found)?;
    let text = String::from_utf8(store.get(&artifact).await.map_err(failed)?).ok();
    let not_media = || {
        query_fault(
            codes::PRECONDITION_NOT_MEDIA,
            "the attachment is not a data URL",
        )
    };
    let parsed = text
        .as_deref()
        .and_then(data_url::parse)
        .ok_or_else(not_media)?;
    let bytes = STANDARD.decode(parsed.data).map_err(|_| not_media())?;
    Ok((bytes, parsed.media_type.to_owned()))
}

fn ok(data: Value) -> Value {
    json!(QueryResult::Ok(QueryResultOk { data, cursor: None }))
}

/// 读面的来源：上传的 Artifact（§39.4），或会话主 env 上的本机文件（§41.4，经入口的端口）。
enum Source {
    Upload(Arc<dyn ArtifactStore>),
    Local {
        workspaces: Arc<dyn Workspaces>,
        key: String,
    },
}

fn local_fault(error: AttachmentFileError) -> Fault {
    match error {
        AttachmentFileError::NotFound => {
            query_fault(codes::PRECONDITION_NOT_FOUND, "the attached file is gone")
        }
        AttachmentFileError::TooLarge { size } => query_fault(
            codes::PRECONDITION_TOO_LARGE,
            format!("the attached file is {size} bytes"),
        ),
        AttachmentFileError::Failed(message) => query_fault(codes::INTERNAL_FAILED, message),
        AttachmentFileError::Unsupported => query_fault(
            codes::PRECONDITION_CAPABILITY_UNSUPPORTED,
            "local attachments are not read here",
        ),
    }
}

/// 读本机附件的上限（Node `readAttachmentPayload`）：视频大一些。
fn read_limit(row_type: &str) -> u64 {
    if MediaKind::of(row_type) == Some(MediaKind::Video) {
        ATTACHMENT_PREVIEW_MAX_BYTES
    } else {
        ATTACHMENT_READ_MAX_BYTES
    }
}

/// 读面读出的东西要能预览（Node `previewNotMedia`）：图片、视频、PDF，或者行与读出的都是 `text/plain`。
fn readable(media_type: &str, row_type: &str) -> bool {
    let plain = |value: &str| {
        value
            .split(';')
            .next()
            .is_some_and(|base| base.trim().eq_ignore_ascii_case(mime::TEXT_PLAIN))
    };
    MediaKind::of(media_type).is_some() || (plain(media_type) && plain(row_type))
}

/// `attachment.read`：读出整份、核对类型，按偏移切一块（§39.4、§41.4）。本机文件的类型是行的类型。
async fn read_chunk(
    source: Source,
    (reference, row_type): (String, String),
    (offset, length): (u64, u64),
) -> Result<Value, Fault> {
    let (bytes, media_type) = match source {
        Source::Upload(store) => read_upload(store, &reference).await?,
        Source::Local { workspaces, key } => {
            let limit = read_limit(&row_type);
            let bytes = workspaces
                .read_attachment(&key, &reference, limit)
                .await
                .map_err(local_fault)?;
            (bytes, row_type.clone())
        }
    };
    if !readable(&media_type, &row_type) {
        return Err(query_fault(
            codes::PRECONDITION_NOT_MEDIA,
            format!("{media_type} cannot be previewed"),
        ));
    }
    let total = bytes.len() as u64;
    if offset > total {
        return Err(query_fault(
            codes::PRECONDITION_RANGE_INVALID,
            format!("offset {offset} is past the end ({total} bytes)"),
        ));
    }
    let end = offset.saturating_add(length).min(total);
    let chunk = &bytes
        [usize::try_from(offset).unwrap_or(usize::MAX)..usize::try_from(end).unwrap_or(usize::MAX)];
    Ok(ok(json!(QueryAttachmentReadResult {
        data_base64: STANDARD.encode(chunk),
        media_type,
        total_bytes: total,
        next_offset: (end < total).then_some(end),
    })))
}

/// `attachment.stat`：上传的是解码之后的大小与类型；本机文件只 stat（行的类型、文件大小与修改时间）。
async fn stat(source: Source, (reference, row_type): (String, String)) -> Result<Value, Fault> {
    let result = match source {
        Source::Upload(store) => {
            let (bytes, media_type) = read_upload(store, &reference).await?;
            QueryAttachmentStatResult {
                media_type,
                total_bytes: bytes.len() as u64,
                mtime_ms: None,
            }
        }
        Source::Local { workspaces, key } => {
            let file = workspaces
                .stat_attachment(&key, &reference)
                .await
                .map_err(local_fault)?;
            QueryAttachmentStatResult {
                media_type: row_type,
                total_bytes: file.size,
                mtime_ms: Some(file.mtime_ms as f64),
            }
        }
    };
    Ok(ok(json!(result)))
}

impl Actor {
    /// 一条上传命令（§39.3）。ACK 排进出站队列；commit 的在提交任务回来时排。
    pub(crate) fn attachment_command(&mut self, reply: &Value, envelope: &CommandEnvelope) {
        let outcome = match self.upload_step(reply, envelope) {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(result) => *result,
        };
        self.outbox.reply(reply.clone(), Ok(json!(outcome)));
    }

    fn upload_step(
        &mut self,
        reply: &Value,
        envelope: &CommandEnvelope,
    ) -> Result<Option<CommandResult>, Box<CommandResult>> {
        crate::session::commands::workspace_key(envelope)?;
        let session = crate::session::commands::session_id(envelope)?;
        let kind = envelope.r#type.as_str();
        match kind {
            ATTACHMENT_BEGIN => self.begin(&session, envelope).map(Some),
            ATTACHMENT_CHUNK => self
                .chunk(&session, &command_payload(kind, &envelope.payload)?)
                .map(Some),
            ATTACHMENT_COMMIT => {
                self.commit(reply, &session, command_payload(kind, &envelope.payload)?)
            }
            _ => Ok(Some(self.abort_upload(
                &session,
                &command_payload::<CommandAttachmentAbort>(kind, &envelope.payload)?,
            ))),
        }
    }

    /// begin：只登记（§39.3）。uploadId 就是这条命令的 id：重放按它认（01 §17.3 的 client 生成 ID）。
    fn begin(
        &mut self,
        session: &str,
        envelope: &CommandEnvelope,
    ) -> Result<CommandResult, Box<CommandResult>> {
        let begin: CommandAttachmentBegin = command_payload(ATTACHMENT_BEGIN, &envelope.payload)?;
        if begin.name.contains(NAME_FORBIDDEN) {
            return Err(refused(
                codes::PRECONDITION_INVALID_PAYLOAD,
                Some(NAME_FIELD),
                "the name must not contain line breaks or NUL",
            ));
        }
        let id = envelope.command_id.clone();
        let started = || {
            accepted(
                Some(json!(AttachmentBeginResult {
                    upload_id: id.clone()
                })),
                false,
            )
        };
        if let Some(upload) = self.attachments.uploads.get(&id) {
            return if upload.session == session && upload.begun == begin {
                Ok(started())
            } else {
                Err(refused(
                    codes::PRECONDITION_COMMAND_ID_CONFLICT,
                    None,
                    "this command id began another upload",
                ))
            };
        }
        let Some(store) = self.attachments.store.clone() else {
            return Err(refused(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                None,
                "the store is not ready yet",
            ));
        };
        if self.attachments.active() >= UPLOAD_MAX_CONCURRENT {
            return Err(refused(
                codes::PRECONDITION_TOO_MANY_UPLOADS,
                None,
                "too many uploads at once",
            ));
        }
        let result = started();
        let capacity = usize::try_from(begin.size).unwrap_or_default();
        self.attachments.uploads.insert(
            id,
            Upload {
                session: session.to_owned(),
                begun: begin,
                store,
                state: State::Staging(Vec::with_capacity(capacity)),
            },
        );
        Ok(result)
    }

    /// chunk：按偏移追加（§39.3）。重传已收的同一段字节是 noop。
    fn chunk(
        &mut self,
        session: &str,
        chunk: &CommandAttachmentChunk,
    ) -> Result<CommandResult, Box<CommandResult>> {
        let bytes = STANDARD
            .decode(&chunk.bytes)
            .ok()
            .filter(|bytes| bytes.len() <= CHUNK_MAX_BYTES)
            .ok_or_else(|| {
                refused(
                    codes::PRECONDITION_INVALID_PAYLOAD,
                    Some(BYTES_FIELD),
                    &format!("bytes must be base64 of at most {CHUNK_MAX_BYTES} bytes"),
                )
            })?;
        let staged_total = self.attachments.staged;
        let Some(upload) = self.attachments.get(session, &chunk.upload_id) else {
            return Err(refused(
                codes::PRECONDITION_NOT_FOUND,
                None,
                "no such upload",
            ));
        };
        let size = upload.begun.size;
        let staged = match &mut upload.state {
            State::Staging(staged) => staged,
            State::Committing(_) => {
                return Err(refused(
                    codes::PRECONDITION_COMMIT_IN_PROGRESS,
                    None,
                    "the upload is being committed",
                ));
            }
            // 提交之前的一块重放：原结果是 accepted
            State::Committed(_) => return Ok(accepted(None, true)),
        };
        let offset = usize::try_from(chunk.offset).unwrap_or(usize::MAX);
        let received = staged.len();
        if offset > received {
            return Err(refused(
                codes::PRECONDITION_CHUNK_GAP,
                None,
                &format!("expected offset {received}"),
            ));
        }
        if offset < received {
            let end = offset.saturating_add(bytes.len());
            return if end <= received && staged[offset..end] == bytes[..] {
                Ok(accepted(None, true))
            } else {
                Err(refused(
                    codes::PRECONDITION_CHUNK_CONFLICT,
                    None,
                    "the bytes differ from those already received",
                ))
            };
        }
        if (received + bytes.len()) as u64 > size {
            return Err(refused(
                codes::PRECONDITION_SIZE_EXCEEDED,
                None,
                &format!("the upload is {size} bytes"),
            ));
        }
        // 全部上传的暂存共用一份预算（Node 在 chunk 时守）：单个可到 20 MiB 之后，16 个同时上传会超过它
        if staged_total + bytes.len() > UPLOAD_MAX_STAGED_BYTES {
            return Err(refused(
                codes::PRECONDITION_STAGING_CAPACITY_EXCEEDED,
                None,
                &format!("uploads may stage at most {UPLOAD_MAX_STAGED_BYTES} bytes at once"),
            ));
        }
        staged.extend_from_slice(&bytes);
        self.attachments.staged += bytes.len();
        Ok(accepted(None, false))
    }

    /// commit：收全了就交给提交任务（§39.3）；ACK 等它回来。已提交的回原结果，提交中的这一条等同一次结果。
    fn commit(
        &mut self,
        reply: &Value,
        session: &str,
        commit: CommandAttachmentCommit,
    ) -> Result<Option<CommandResult>, Box<CommandResult>> {
        let Some(upload) = self.attachments.get(session, &commit.upload_id) else {
            return Err(refused(
                codes::PRECONDITION_NOT_FOUND,
                None,
                "no such upload",
            ));
        };
        let bytes = match &mut upload.state {
            State::Committed(result) => return Ok(Some(accepted(Some(json!(result)), false))),
            State::Committing(waiting) => {
                waiting.push(reply.clone());
                return Ok(None);
            }
            State::Staging(staged) if staged.len() as u64 != upload.begun.size => {
                return Err(refused(
                    codes::PRECONDITION_UPLOAD_INCOMPLETE,
                    None,
                    &format!("{} of {} bytes received", staged.len(), upload.begun.size),
                ));
            }
            State::Staging(staged) => std::mem::take(staged),
        };
        upload.state = State::Committing(vec![reply.clone()]);
        let declared = upload.begun.mime.clone();
        let (store, events) = (upload.store.clone(), self.events.clone());
        let owner = (SessionId::new(session), commit.upload_id.clone());
        self.attachments.in_flight += 1;
        drop(
            self.spawner
                .spawn("protocol-v5-attachment-commit", async move {
                    let upload = owner.1.clone();
                    let outcome =
                        commit_bytes(store, owner, (bytes, declared), commit.sha256).await;
                    let _ = events.send(Event::AttachmentCommitted { upload, outcome });
                }),
        );
        Ok(None)
    }

    /// abort：删掉暂存（§39.3）。提交中拒绝；已提交或没有是 noop（Artifact 不删：引用可能已经发出去）。
    fn abort_upload(&mut self, session: &str, abort: &CommandAttachmentAbort) -> CommandResult {
        match self
            .attachments
            .get(session, &abort.upload_id)
            .map(|upload| &upload.state)
        {
            Some(State::Staging(staged)) => {
                self.attachments.staged -= staged.len();
                self.attachments.uploads.remove(&abort.upload_id);
                accepted(None, false)
            }
            Some(State::Committing(_)) => *refused(
                codes::PRECONDITION_COMMIT_IN_PROGRESS,
                None,
                "the upload is being committed",
            ),
            Some(State::Committed(_)) | None => accepted(None, true),
        }
    }

    /// 提交任务回来了：成功记下结果，失败删掉这个上传；等着的回复都按结果排（§39.3）。
    pub(crate) fn attachment_committed(
        &mut self,
        upload: &str,
        outcome: Result<AttachmentCommitResult, CommitFailure>,
    ) {
        self.attachments.in_flight -= 1;
        // 只有提交中的上传有提交任务，提交中的上传不会被删（abort 拒绝它）
        if let Some(entry) = self.attachments.uploads.get_mut(upload)
            && let State::Committing(waiting) = &mut entry.state
        {
            let waiting = std::mem::take(waiting);
            // 提交中的字节到这里才离开内存（在提交任务里）：成功、失败都放出预算。提交时已收全，所以就是声明的大小
            self.attachments.staged -= usize::try_from(entry.begun.size).unwrap_or_default();
            let result: Result<Value, RpcError> = match outcome {
                Ok(result) => {
                    let value = json!(accepted(Some(json!(result)), false));
                    entry.state = State::Committed(result);
                    Ok(value)
                }
                Err(failure) => {
                    self.attachments.uploads.remove(upload);
                    match failure {
                        CommitFailure::Rejected { code, message } => {
                            Ok(json!(rejected(code, None, Some(message))))
                        }
                        CommitFailure::Failed(message) => {
                            Err(to_rpc(&protocol_error(codes::INTERNAL_FAILED, message)))
                        }
                    }
                }
            };
            for reply in waiting {
                self.outbox.reply(reply, result.clone());
            }
        }
        self.finish_shutdown();
    }

    /// 一条读面查询（§39.4）：先按会话的投影授权；`previewSource` 不读文件、直接回；`read` / `stat` 交给读面任务。
    pub(crate) fn attachment_query(
        &mut self,
        reply: &Value,
        query: QueryEnvelope,
    ) -> Result<Option<Value>, Fault> {
        let Some(key) = query.target.workspace_key.as_deref() else {
            return Err(query_fault(
                codes::PRECONDITION_WORKSPACE_REQUIRED,
                "attachment queries need target.workspaceKey",
            ));
        };
        local_workspace(key)
            .map_err(|message| query_fault(codes::PRECONDITION_INVALID, message))?;
        let Some(session) = query
            .target
            .session_id
            .as_deref()
            .filter(|sid| !sid.is_empty())
        else {
            return Err(query_fault(
                codes::PRECONDITION_INVALID_PAYLOAD,
                "attachment queries need target.sessionId",
            ));
        };
        let kind = query.r#type.as_str();
        let params = query.params.clone().unwrap_or(Value::Null);
        let invalid = |error: String| {
            let (field, message) = field_path(&error);
            query_fault(
                codes::PRECONDITION_INVALID_PAYLOAD,
                format!("{field}: {message}"),
            )
        };
        let schema = format!("{QUERY_SCHEMA_PREFIX}{kind}{QUERY_SCHEMA_SUFFIX}");
        let (located, offset_length) = if kind == ATTACHMENT_READ {
            let read: QueryAttachmentReadParams = decode(schema, &params).map_err(invalid)?;
            (
                (read.r#ref, read.row_id, read.attachment_index),
                Some((read.offset, read.length)),
            )
        } else {
            let locate: QueryAttachmentPreviewSourceParams =
                decode(schema, &params).map_err(invalid)?;
            ((locate.r#ref, locate.row_id, locate.attachment_index), None)
        };
        let (reference, row, index) = located;
        let at = match (row.as_deref(), index) {
            (Some(row), Some(index)) => Some((row, usize::try_from(index).unwrap_or(usize::MAX))),
            (None, None) => None,
            _ => {
                return Err(query_fault(
                    codes::PRECONDITION_INVALID_PAYLOAD,
                    "rowId and attachmentIndex come together",
                ));
            }
        };
        let not_authorized = || {
            query_fault(
                codes::PRECONDITION_NOT_AUTHORIZED,
                "no row of this session carries this attachment",
            )
        };
        let topic = session_topic(key, session);
        let mime = self
            .topics
            .get(&topic)
            .and_then(|publisher| crate::session::shown_attachment(publisher, &reference, at))
            .ok_or_else(not_authorized)?;
        if kind == ATTACHMENT_PREVIEW_SOURCE {
            let local =
                Path::new(&reference).is_absolute() && mime.to_ascii_lowercase().starts_with(VIDEO);
            let source = if local {
                QueryAttachmentPreviewSourceResult::LocalPath(
                    QueryAttachmentPreviewSourceResultLocalPath {
                        path: reference,
                        media_type: mime,
                    },
                )
            } else {
                QueryAttachmentPreviewSourceResult::Chunked(
                    QueryAttachmentPreviewSourceResultChunked {},
                )
            };
            return Ok(Some(ok(json!(source))));
        }
        let unavailable = || {
            query_fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the store is not ready yet",
            )
        };
        // 行里不是 Artifact URI 的引用就是本机的绝对路径（input.send 只收这两种，§15.4.1）：经入口读会话主 env 上的文件（§41.4）
        let source = if artifact_uri::parse(&reference).is_some() {
            Source::Upload(self.attachments.store.clone().ok_or_else(unavailable)?)
        } else {
            Source::Local {
                workspaces: self.process.workspaces.clone().ok_or_else(unavailable)?,
                key: key.to_owned(),
            }
        };
        let (events, reply) = (self.events.clone(), reply.clone());
        self.attachments.in_flight += 1;
        drop(
            self.spawner
                .spawn("protocol-v5-attachment-read", async move {
                    let located = (reference, mime);
                    let result = match offset_length {
                        Some(range) => read_chunk(source, located, range).await,
                        None => stat(source, located).await,
                    };
                    let _ = events.send(Event::AttachmentRead { reply, result });
                }),
        );
        Ok(None)
    }

    /// 读面任务回来了：直接写出回复（同 `sessions.facts`：查询的结果与帧不保证先后，§14.6）。
    pub(crate) async fn attachment_read(&mut self, reply: Value, result: Result<Value, Fault>) {
        self.attachments.in_flight -= 1;
        self.peer
            .respond(&reply, &result.map_err(|error| to_rpc(&error)))
            .await;
        self.finish_shutdown();
    }
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;
