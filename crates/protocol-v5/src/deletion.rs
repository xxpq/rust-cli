//! 归档会话的永久删除（protocol-v5.md §65；02 §2.6）：Host 专用的查询 `sessionData.deletionPreview` 与命令 `sessionData.deletionExecute`。
//! 校验在 actor 里；预检交给查询的任务（同 `sessions.facts`），执行交给工作区级写入的工作任务（[`crate::trust_writes`]，按到达顺序一条一条做）。
//! 内核在任务里组装工作区（`Workspaces::open`）再答：打开着的会话只在它工作区的内核里，占住条目要那一套的会话表。
//!
//! ```text
//! query  sessionData.deletionPreview ─ 校验 ─► 任务: open(key) → kernel.deletion_preview ─► Event::Queried ─► 直接回复
//! command sessionData.deletionExecute ─ 校验 ─► 工作任务: open(key) → kernel.delete_permanently ─► Event::Deleted
//!   actor: deleted → feed 每个会话 entity.remove、打开着的会话 topic 标成 deleted → 排 ACK（效果之后，G2）
//! ```

use std::sync::OnceLock;

use rust_cli_kernel_api::deletion::{
    DeletionBlock, DeletionOutcome, DeletionPreview, DeletionResult, RetainedArtifact,
    TitledSession,
};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_protocol_core::schema::Node;
use serde_json::Value;

use crate::commands::{command_schema, invalid_payload, rejected, workspace_key};
use crate::errors::{Fault, protocol_error};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandResultAccepted, CommandResultAcceptedEffectsItem,
    QueryEnvelope, QueryResult, QueryResultOk,
};
use crate::generated::session_deletion_v1::{
    CommandResultSessionDataDeletionExecute, CommandResultSessionDataDeletionExecuteStatus,
    CommandSessionDataDeletionExecute, QuerySessionDataDeletionPreviewParams,
    QuerySessionDataDeletionPreviewResult, QuerySessionDataDeletionPreviewResultBlockedReason,
    QuerySessionDataDeletionPreviewResultForksItem,
    QuerySessionDataDeletionPreviewResultRetainedArtifactsItem,
};
use crate::server::{Actor, Event, Workspaces};
use crate::topics::{WORKSPACE_SESSIONS, workspace_topic};

/// 预检的查询类型与执行的命令类型（session.deletion@1）。
pub(crate) const DELETION_PREVIEW: &str = "sessionData.deletionPreview";
pub(crate) const DELETION_EXECUTE: &str = "sessionData.deletionExecute";
/// 给 rust 的 session.deletion@1 schema（生成物）里参数的 schema 名。
const PREVIEW_SCHEMA: &str = "Query.sessionData.deletionPreview.params";
const EXECUTE_SCHEMA: &str = "Command.sessionData.deletionExecute";
const SCHEMA: &str = include_str!("../schema/session.deletion@1.json");
/// 会话列表的集合名（04 §3 的 `facts`，同 sessions.rs）。
const FACTS: &str = "facts";

fn preview_schema() -> Option<&'static Node> {
    static NODE: OnceLock<Option<Node>> = OnceLock::new();
    command_schema(&NODE, SCHEMA, PREVIEW_SCHEMA)
}

fn execute_schema() -> Option<&'static Node> {
    static NODE: OnceLock<Option<Node>> = OnceLock::new();
    command_schema(&NODE, SCHEMA, EXECUTE_SCHEMA)
}

fn fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

fn titled(sessions: &[TitledSession]) -> Vec<QuerySessionDataDeletionPreviewResultForksItem> {
    sessions
        .iter()
        .map(|titled| QuerySessionDataDeletionPreviewResultForksItem {
            session_id: titled.session.as_str().to_owned(),
            title: titled.title.clone(),
        })
        .collect()
}

fn retained(
    retained: &RetainedArtifact,
) -> QuerySessionDataDeletionPreviewResultRetainedArtifactsItem {
    QuerySessionDataDeletionPreviewResultRetainedArtifactsItem {
        session_id: retained.session.as_str().to_owned(),
        artifact_id: retained.artifact.clone(),
        file_name: retained.file_name.clone(),
        referrers: titled(&retained.referrers),
    }
}

fn block_reason(block: DeletionBlock) -> QuerySessionDataDeletionPreviewResultBlockedReason {
    match block {
        DeletionBlock::Running => QuerySessionDataDeletionPreviewResultBlockedReason::Running,
        DeletionBlock::Changed => QuerySessionDataDeletionPreviewResultBlockedReason::Changed,
        DeletionBlock::NotFound => QuerySessionDataDeletionPreviewResultBlockedReason::NotFound,
        // 前端在换算之前就把它当前置条件拒绝了（[`other_workspace`]）
        DeletionBlock::OtherWorkspace => {
            QuerySessionDataDeletionPreviewResultBlockedReason::Unavailable
        }
    }
}

const OTHER_WORKSPACE: &str = "the session belongs to another workspace";

/// 内核的预检 → 契约的结果（02 §2.6）。`incomplete` / `completed` 来自 Node 的删除凭据。
fn preview_result(root: &str, preview: &DeletionPreview) -> QuerySessionDataDeletionPreviewResult {
    QuerySessionDataDeletionPreviewResult {
        session_id: root.to_owned(),
        revision: preview.revision.clone(),
        session_ids: ids(&preview.sessions),
        forks: titled(&preview.forks),
        retained_artifacts: preview.retained.iter().map(retained).collect(),
        deleted_files: u64::try_from(preview.deleted_files).unwrap_or(u64::MAX),
        blocked_reason: preview.blocked.map(block_reason),
        incomplete: preview.incomplete,
        completed: preview.completed,
    }
}

fn ids(sessions: &[SessionId]) -> Vec<String> {
    sessions
        .iter()
        .map(|session| session.as_str().to_owned())
        .collect()
}

/// `sessionData.deletionPreview`（§65.1）：外壳的校验、组装工作区与预检都在任务里，错误也走同一条回复路径（同 `sessions.facts`）。
async fn preview(
    workspaces: Option<std::sync::Arc<dyn Workspaces>>,
    query: QueryEnvelope,
) -> Result<Value, Fault> {
    let Some(key) = query.target.workspace_key.clone() else {
        return Err(fault(
            codes::PRECONDITION_WORKSPACE_REQUIRED,
            "sessionData.deletionPreview needs target.workspaceKey",
        ));
    };
    crate::topics::local_workspace(&key)
        .map_err(|message| fault(codes::PRECONDITION_INVALID, message))?;
    let params = query.params.unwrap_or(Value::Null);
    if let Some(schema) = preview_schema()
        && let Err(error) = schema.validate(&params, "")
    {
        let (field, message) = crate::commands::field_path(&error);
        return Err(fault(
            codes::PRECONDITION_INVALID_PAYLOAD,
            format!("{field}: {message}"),
        ));
    }
    let params: QuerySessionDataDeletionPreviewParams = serde_json::from_value(params)
        .map_err(|error| fault(codes::PRECONDITION_INVALID_PAYLOAD, error.to_string()))?;
    let Some(workspaces) = workspaces else {
        return Err(fault(
            codes::PRECONDITION_STORE_UNAVAILABLE,
            "the session store is not ready yet",
        ));
    };
    let kernel = workspaces
        .open(&key)
        .await
        .map_err(|message| fault(codes::INTERNAL_FAILED, message))?;
    let root = SessionId::new(&params.session_id);
    let preview = kernel
        .kernel
        .deletion_preview(&root, &workspaces.workspace(&key))
        .await
        .map_err(|error| fault(codes::INTERNAL_FAILED, error.to_string()))?;
    if preview.blocked == Some(DeletionBlock::OtherWorkspace) {
        return Err(fault(
            codes::PRECONDITION_WORKSPACE_MISMATCH,
            OTHER_WORKSPACE,
        ));
    }
    let data =
        serde_json::to_value(preview_result(&params.session_id, &preview)).unwrap_or(Value::Null);
    Ok(
        serde_json::to_value(QueryResult::Ok(QueryResultOk { data, cursor: None }))
            .unwrap_or(Value::Null),
    )
}

/// 执行的结局 → 契约的结果；`OtherWorkspace` 是前置条件的拒绝（什么都没删）。
fn execute_result(root: &str, result: &DeletionResult) -> CommandResult {
    let (status, reason) = match &result.outcome {
        DeletionOutcome::Deleted => (CommandResultSessionDataDeletionExecuteStatus::Deleted, None),
        DeletionOutcome::Blocked(DeletionBlock::OtherWorkspace) => {
            return rejected(
                codes::PRECONDITION_WORKSPACE_MISMATCH,
                None,
                Some(OTHER_WORKSPACE.to_owned()),
            );
        }
        DeletionOutcome::Blocked(block) => (
            CommandResultSessionDataDeletionExecuteStatus::Blocked,
            Some(block_reason(*block).as_str().to_owned()),
        ),
        DeletionOutcome::Incomplete(reason) => (
            CommandResultSessionDataDeletionExecuteStatus::Incomplete,
            Some(reason.clone()),
        ),
    };
    let outcome = CommandResultSessionDataDeletionExecute {
        session_id: root.to_owned(),
        status,
        session_ids: ids(&result.sessions),
        retained_artifacts: u64::try_from(result.retained).unwrap_or(u64::MAX),
        reason,
    };
    CommandResult::Accepted(CommandResultAccepted {
        effects: Vec::new(),
        noop: false,
        result: Some(serde_json::to_value(outcome).unwrap_or(Value::Null)),
    })
}

impl Actor {
    /// `sessionData.deletionPreview`（§65.1）：交给查询的任务，回复等 [`Event::Queried`]。
    pub(crate) fn deletion_preview(&mut self, reply: &Value, query: QueryEnvelope) {
        let workspaces = self.process.workspaces.clone();
        self.answer_in_task(
            "protocol-v5-deletion-preview",
            reply,
            preview(workspaces, query),
        );
    }

    /// `sessionData.deletionExecute`（§65.1）：校验，通过的交给工作任务、返回 None（ACK 等 [`Event::Deleted`]）；没通过的返回结果。
    pub(crate) fn delete_permanently(
        &mut self,
        reply: &Value,
        command: &CommandEnvelope,
    ) -> Option<CommandResult> {
        let key = match workspace_key(command) {
            Ok(key) => key,
            Err(result) => return Some(*result),
        };
        if let Some(schema) = execute_schema()
            && let Err(error) = schema.validate(&command.payload, "")
        {
            return Some(invalid_payload(&error));
        }
        let payload = match serde_json::from_value::<CommandSessionDataDeletionExecute>(
            command.payload.clone(),
        ) {
            Ok(payload) => payload,
            Err(error) => {
                return Some(rejected(
                    codes::PRECONDITION_INVALID_PAYLOAD,
                    Some(String::new()),
                    Some(error.to_string()),
                ));
            }
        };
        let Some(workspaces) = self.process.workspaces.clone() else {
            return Some(rejected(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                None,
                Some("the process is not ready yet".to_owned()),
            ));
        };
        let reply = reply.clone();
        // 删除凭据的 operation id 是这条命令的 commandId（v5 的命令身份就是意图的身份，02 §2.6）
        let operation = command.command_id.clone();
        let job = Box::pin(async move {
            let workspace = workspaces.workspace(&key);
            let root = SessionId::new(&payload.session_id);
            let result = match workspaces.open(&key).await {
                Ok(kernel) => kernel
                    .kernel
                    .delete_permanently(&root, &workspace, &payload.revision, &operation)
                    .await
                    .map_err(|error| error.to_string()),
                Err(message) => Err(message),
            };
            Event::Deleted {
                reply,
                key,
                root: payload.session_id,
                result,
            }
        });
        self.trusts.submit(job, &self.events, &self.spawner);
        None
    }

    /// 工作任务做完一条（§65.1）：删了的先从会话列表移除、打开着的会话 topic 标成 deleted（效果），再排 ACK（G2）。
    pub(crate) fn deleted(
        &mut self,
        reply: Value,
        key: &str,
        root: &str,
        result: Result<DeletionResult, String>,
    ) {
        let value = match result {
            Ok(result) => {
                let mut effects = Vec::new();
                if result.outcome == DeletionOutcome::Deleted {
                    let topic = workspace_topic(key, WORKSPACE_SESSIONS);
                    if let Some(publisher) = self.topics.get_mut(&topic) {
                        let mut batch = crate::publisher::Batch::default();
                        for session in &result.sessions {
                            batch.remove(FACTS, session.as_str());
                        }
                        if publisher.publish(batch) {
                            self.outbox.mark_topic(&topic);
                        }
                        let cursor = publisher.cursor();
                        effects.push(CommandResultAcceptedEffectsItem {
                            topic,
                            epoch: cursor.epoch,
                            seq: cursor.seq,
                        });
                    }
                    for session in &result.sessions {
                        self.mark_deleted(session.as_str());
                    }
                }
                let mut outcome = execute_result(root, &result);
                if let CommandResult::Accepted(accepted) = &mut outcome {
                    accepted.effects = effects;
                }
                Ok(serde_json::to_value(outcome).unwrap_or(Value::Null))
            }
            Err(message) => Err(crate::errors::to_rpc(&protocol_error(
                codes::INTERNAL_FAILED,
                message,
            ))),
        };
        self.outbox.reply(reply, value);
        self.trusts.done();
        self.finish_shutdown();
    }
}

#[cfg(test)]
#[path = "deletion_tests.rs"]
mod tests;
