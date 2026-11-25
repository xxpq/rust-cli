//! 排版之前的摘要：003670f750e5a844
//! 生成物，不要手改：`attachments@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentBeginResult {
    #[serde(rename = "uploadId")]
    pub upload_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentCommitResult {
    #[serde(rename = "attachmentId")]
    pub attachment_id: String,
    pub r#ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub r#ref: String,
    #[serde(rename = "fileName", default = "default_attachment_ref_file_name")]
    pub file_name: String,
    #[serde(default = "default_attachment_ref_mime")]
    pub mime: String,
    #[serde(default = "default_attachment_ref_bytes")]
    pub bytes: f64,
    #[serde(
        rename = "previewRef",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub preview_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAttachmentAbort {
    #[serde(rename = "uploadId")]
    pub upload_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAttachmentBegin {
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAttachmentChunk {
    #[serde(rename = "uploadId")]
    pub upload_id: String,
    pub offset: u64,
    pub bytes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAttachmentCommit {
    #[serde(rename = "uploadId")]
    pub upload_id: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAttachmentPreviewSourceParams {
    pub r#ref: String,
    #[serde(rename = "rowId", default, skip_serializing_if = "Option::is_none")]
    pub row_id: Option<String>,
    #[serde(
        rename = "attachmentIndex",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub attachment_index: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum QueryAttachmentPreviewSourceResult {
    #[serde(rename = "local_path")]
    LocalPath(QueryAttachmentPreviewSourceResultLocalPath),
    #[serde(rename = "chunked")]
    Chunked(QueryAttachmentPreviewSourceResultChunked),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAttachmentPreviewSourceResultLocalPath {
    pub path: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAttachmentPreviewSourceResultChunked {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAttachmentReadParams {
    pub r#ref: String,
    #[serde(rename = "rowId", default, skip_serializing_if = "Option::is_none")]
    pub row_id: Option<String>,
    #[serde(
        rename = "attachmentIndex",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub attachment_index: Option<u64>,
    #[serde(default = "default_query_attachment_read_params_offset")]
    pub offset: u64,
    #[serde(default = "default_query_attachment_read_params_length")]
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAttachmentReadResult {
    #[serde(rename = "dataBase64")]
    pub data_base64: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    #[serde(rename = "nextOffset")]
    pub next_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAttachmentStatResult {
    #[serde(rename = "mediaType")]
    pub media_type: String,
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    #[serde(rename = "mtimeMs", default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<f64>,
}

pub type QueryAttachmentStatParams = QueryAttachmentPreviewSourceParams;

fn default_attachment_ref_file_name() -> String {
    String::new()
}

fn default_attachment_ref_mime() -> String {
    "application/octet-stream".to_owned()
}

fn default_attachment_ref_bytes() -> f64 {
    0.0
}

fn default_query_attachment_read_params_offset() -> u64 {
    0
}

const DEFAULT_QUERY_ATTACHMENT_READ_PARAMS_LENGTH: u64 = 524288;
fn default_query_attachment_read_params_length() -> u64 {
    DEFAULT_QUERY_ATTACHMENT_READ_PARAMS_LENGTH
}
