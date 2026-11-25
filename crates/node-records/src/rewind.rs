//! 回退与 checkpoint 的 Node 记录（docs/specs/rust-cli/session-lifecycle.md §4.2、§5.3）。
//!
//! - `RevertSet` → Node `setRevert`：保留前缀、切点与代数按 Node `buildConversationRewindPlan`
//!   从库里算（库是唯一事实源；工具结果没有 message 行，压缩前的旧行内核不在内存里）。
//! - `CheckpointCreated` → `runtime/workspace_checkpoint`（Node `persistWorkspaceCheckpointEntry`）。
//! - `CheckpointsRestored` → 每个受影响的回合一条 `runtime/workspace_file_rewind`
//!   （Node `persistWorkspaceFileRewindEntry`，`reason = file_summary_rewind`）。
//! - 远端机器上的 checkpoint（R3，docs/specs/rust-cli/remote-exec.md §17.1、§20.9）：同样的形状，类型换成本工程的
//!   `rust-cli/remote_checkpoint` 与 `rust-cli/remote_file_rewind`，`rustCli` 里多 `machine`。Node / App 按类型查询，看不到
//!   它们，App 回退不会把远端文件的内容写到本机的同名路径；序号在这两种记录之间单独算。
use std::borrow::Cow;

use rust_cli_kernel_api::artifact_uri;
use rust_cli_kernel_api::ids::{ArtifactId, MachineName, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::store::{ArtifactRef, RevertCursor, StoreError, WorkspaceCheckpoint};
use serde_json::{Value, json};

use crate::branch::{Branch, select_branch};
use crate::facts::{Commit, save_entry};
use crate::keys;
use crate::rows::Rows;
use crate::state;
use crate::{records, storage};

pub const CHECKPOINT_ENTRY: &str = "runtime/workspace_checkpoint";
pub const FILE_REWIND_ENTRY: &str = "runtime/workspace_file_rewind";

/// 一种记录：Node 的（本机）或本工程的（远端机器上的 checkpoint 与文件回退，R3；种类在 `keys`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Node,
    Remote,
}

impl Kind {
    fn of(machine: &MachineName) -> Self {
        if machine.is_primary() {
            Self::Node
        } else {
            Self::Remote
        }
    }

    fn checkpoint(self) -> Cow<'static, str> {
        match self {
            Self::Node => Cow::Borrowed(CHECKPOINT_ENTRY),
            Self::Remote => Cow::Owned(keys::named(keys::REMOTE_CHECKPOINT)),
        }
    }

    fn file_rewind(self) -> Cow<'static, str> {
        match self {
            Self::Node => Cow::Borrowed(FILE_REWIND_ENTRY),
            Self::Remote => Cow::Owned(keys::named(keys::REMOTE_FILE_REWIND)),
        }
    }
}

/// Node `setRevert` 的值（键序同 Node）。
/// 会话在库里的分支切点（`StoreQueries::branch_cut`，session-facts.md §2）：会话行的 `revert` 原文（编码同写入），没有切点为 None；
/// 删除了的照样给，会话不在为 `NotFound`。
pub fn branch_cut(conn: &dyn Rows, session: &SessionId) -> Result<Option<String>, StoreError> {
    let row = conn
        .session(session.as_str())?
        .ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
    Ok(row.revert.map(|revert| revert.to_string()))
}

pub fn revert(ctx: &Commit<'_>, cursor: &RevertCursor) -> Result<Value, StoreError> {
    let row = ctx
        .conn
        .session(ctx.session)?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    let records = ctx.conn.messages(ctx.session)?;
    let branch = Branch::from_revert(row.revert.as_ref());
    // 只应用旧游标、不应用压缩边界：目标可以在压缩边界之前（Node 同样处理）。
    let active = select_branch(records.iter().map(Cow::Borrowed).collect(), &branch);
    let target = cursor.target.as_str();
    let index = active
        .iter()
        .position(|record| record.id() == target)
        .ok_or_else(|| {
            storage(format!(
                "rewind target {target} is not in the active branch"
            ))
        })?;
    let kept: Vec<&str> = active[..index].iter().map(|record| record.id()).collect();
    let cut = records
        .last()
        .map(crate::row::WithParts::id)
        .ok_or_else(|| storage("rewind needs at least one message"))?;
    let previous = row
        .revert
        .as_ref()
        .and_then(|value| value["branchGeneration"].as_u64())
        .unwrap_or(0);
    Ok(json!({
        "keptMessageIDs": kept,
        "branchCutAfterMessageID": cut,
        "branchGeneration": (previous + 1).max(cursor.generation),
        "messageID": kept.last().copied().unwrap_or(target),
        "kind": "conversation_rewind",
        "scope": "conversation",
        "targetMessageID": target,
    }))
}

/// 工作区事件的序号（Node `sequenceNumber`，Node 恢复时按它排序、App 按它取增量）：本会话已持久化的
/// checkpoint 与文件回退记录（Node 与本工程写的）里的最大值加一。同一事务里先写的记录立刻可见，所以同一批的
/// 几个 checkpoint 依次加一。修复线 F2：以前用本工程的提交序号（`rust-cli/commit`），与 Node 的事件序号不是一个
/// 空间：混写时顺序错、会重复，同一批的几个 checkpoint 序号也相同。
fn sequence(ctx: &Commit<'_>, kind: Kind) -> Result<u64, StoreError> {
    let (checkpoint, file_rewind) = (kind.checkpoint(), kind.file_rewind());
    let max = ctx
        .conn
        .max_sequence_number(ctx.session, [checkpoint.as_ref(), file_rewind.as_ref()])?;
    Ok(max.map_or(0, |max| u64::try_from(max).unwrap_or(0)) + 1)
}

fn trace(ctx: &Commit<'_>) -> Result<String, StoreError> {
    let row = ctx.conn.session(ctx.session)?;
    Ok(row
        .and_then(|row| row.trace_id)
        .unwrap_or_else(|| ctx.session.to_owned()))
}

/// Node 的事件包装（`PersistedWorkspaceEvent`）；`turnId` 取 user 消息的 `anchor.turnId`。
fn persisted(
    ctx: &Commit<'_>,
    payload: Value,
    prompt: &str,
    kind: Kind,
) -> Result<Value, StoreError> {
    let mut data = json!({
        "eventId": (ctx.uuid)(),
        "payload": payload,
        "sequenceNumber": sequence(ctx, kind)?,
        "traceId": trace(ctx)?,
    });
    if let Some(turn) = state::message(ctx.conn, ctx.session, prompt)?
        .and_then(|info| info["anchor"]["turnId"].as_str().map(str::to_owned))
    {
        data["turnId"] = turn.into();
    }
    Ok(data)
}

pub fn checkpoint(ctx: &Commit<'_>, checkpoint: &WorkspaceCheckpoint) -> Result<(), StoreError> {
    let uri = crate::artifacts::uri(&checkpoint.snapshot);
    let payload = json!({
        "checkpointId": checkpoint.id,
        "messageId": checkpoint.prompt.as_str(),
        "targetMessageId": checkpoint.prompt.as_str(),
        "toolMessageId": checkpoint.assistant.as_str(),
        "scope": "workspace",
        "snapshotRef": uri,
        "diffRef": uri,
        "fileCount": 1,
    });
    let kind = Kind::of(&checkpoint.machine);
    let mut data = persisted(ctx, payload, checkpoint.prompt.as_str(), kind)?;
    // payload 是 Node 的 strict schema；内核读回要的 Artifact 引用与调用 id 放在 payload 之外。
    data["rustCli"] = json!({
        "snapshot": checkpoint.snapshot,
        "toolUse": checkpoint.tool_use.as_str(),
    });
    if let Some(machine) = checkpoint.machine.remote() {
        data["rustCli"]["machine"] = Value::from(machine);
    }
    let event = data["eventId"].as_str().unwrap_or_default().to_owned();
    let id = match kind {
        Kind::Node => format!("workspace-checkpoint:{event}"),
        Kind::Remote => format!("rust-cli-remote-checkpoint:{event}"),
    };
    let entry = records::entry(id, ctx.session, &kind.checkpoint(), ctx.now, data, true);
    save_entry(ctx, &entry)
}

/// 每个受影响的回合（同一 `messageId`）一条文件回退记录，指向该回合最后一个被撤销的 checkpoint。本机与远端的分开写
/// （各自的记录类型）。
pub fn restored(ctx: &Commit<'_>, ids: &[String]) -> Result<(), StoreError> {
    for kind in [Kind::Node, Kind::Remote] {
        restored_of(ctx, ids, kind)?;
    }
    Ok(())
}

fn restored_of(ctx: &Commit<'_>, ids: &[String], kind: Kind) -> Result<(), StoreError> {
    let all = stored(ctx.conn, ctx.session, kind)?;
    let mut turns: Vec<(&str, &Stored)> = Vec::new();
    for checkpoint in all.iter().filter(|c| ids.contains(&c.id)) {
        match turns
            .iter_mut()
            .find(|(prompt, _)| *prompt == checkpoint.prompt)
        {
            Some(turn) => turn.1 = checkpoint,
            None => turns.push((&checkpoint.prompt, checkpoint)),
        }
    }
    for (prompt, last) in turns {
        let rewind = format!("rewind_{}", (ctx.uuid)());
        let payload = json!({
            "rewindId": rewind,
            "scope": "workspace",
            "strategy": "active_chain",
            "targetMessageId": prompt,
            "targetCheckpointId": last.id,
            "restoredSnapshotRef": last.uri,
            "reason": "file_summary_rewind",
        });
        let data = persisted(ctx, payload, prompt, kind)?;
        let id = match kind {
            Kind::Node => format!("workspace-file-rewind:{rewind}"),
            Kind::Remote => format!("rust-cli-remote-file-rewind:{rewind}"),
        };
        let entry = records::entry(id, ctx.session, &kind.file_rewind(), ctx.now, data, true);
        save_entry(ctx, &entry)?;
    }
    Ok(())
}

struct Stored {
    id: String,
    prompt: String,
    uri: String,
    sequence: u64,
    kernel: Option<Active>,
}

/// 冷读取给出的一个 checkpoint：本工程写的已是内核的值；Node 写的还要按产物文件补出引用（`node_checkpoints.rs`）。
#[derive(Debug, Clone, PartialEq)]
pub enum Active {
    Rust(WorkspaceCheckpoint),
    Node(NodeCheckpoint),
}

/// Node `persistWorkspaceCheckpointEntry` 的记录（没有 `data.rustCli`）：引用只有 `snapshotRef` 的 URI。
#[derive(Debug, Clone, PartialEq)]
pub struct NodeCheckpoint {
    pub id: String,
    pub prompt: MessageId,
    pub assistant: MessageId,
    pub session: SessionId,
    pub artifact: ArtifactId,
}

fn stored(conn: &dyn Rows, session: &str, kind: Kind) -> Result<Vec<Stored>, StoreError> {
    let entries = conn.entries(session, Some(&kind.checkpoint()))?;
    let mut out: Vec<Stored> = entries
        .into_iter()
        .filter_map(|entry| {
            let data = entry.data;
            let payload = &data["payload"];
            let id = payload["checkpointId"].as_str()?.to_owned();
            let prompt = payload["targetMessageId"]
                .as_str()
                .or_else(|| payload["messageId"].as_str())?
                .to_owned();
            let assistant = payload["toolMessageId"].as_str().map(MessageId::new);
            let snapshot: Option<ArtifactRef> =
                serde_json::from_value(data["rustCli"]["snapshot"].clone()).ok();
            let tool_use = data["rustCli"]["toolUse"].as_str().map(ToolUseId::new);
            let machine = MachineName::new(data["rustCli"]["machine"].as_str().unwrap_or_default());
            let uri = payload["snapshotRef"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let kernel = match (assistant, snapshot, tool_use) {
                (Some(assistant), Some(snapshot), Some(tool_use)) => {
                    Some(Active::Rust(WorkspaceCheckpoint {
                        machine,
                        id: id.clone(),
                        prompt: MessageId::new(prompt.as_str()),
                        assistant,
                        tool_use,
                        snapshot,
                    }))
                }
                // 修复线 F12：Node 写的没有 `rustCli`，以前一律不给出（不能回退、没有改动摘要）；现在记下 URI
                // 里的会话与 artifact，冷读取再按 Node 的产物文件补出引用。
                // URI 的读法同 Node `parseArtifactUri`（kernel-api `artifact_uri`）；解不出时这个 checkpoint 不给出。
                (Some(assistant), None, None) => {
                    artifact_uri::parse(&uri).map(|(session, artifact)| {
                        Active::Node(NodeCheckpoint {
                            id: id.clone(),
                            prompt: MessageId::new(prompt.as_str()),
                            assistant,
                            session,
                            artifact,
                        })
                    })
                }
                _ => None,
            };
            Some(Stored {
                id,
                prompt,
                uri,
                sequence: data["sequenceNumber"].as_u64().unwrap_or(0),
                kernel,
            })
        })
        .collect();
    // Node `restoreWorkspaceCheckpointEntries`：按 sequenceNumber，再按创建时间（list 已按时间）。
    out.sort_by_key(|stored| stored.sequence);
    Ok(out)
}

/// 冷读取的 checkpoint：还能回退的，与被文件回退撤销过的（它们的回合的文件改动是「已撤销」，回合的 diff 照样从它们算，
/// session-lifecycle.md §10.54）。
#[derive(Debug, Default)]
pub struct Checkpoints {
    pub active: Vec<Active>,
    pub reverted: Vec<Active>,
}

/// 冷读取：按回合的文件回退记录把 checkpoint（两边写的）分成没有撤销的与撤销过的，各按 `sequenceNumber`；本机的在前，再是远端
/// 机器上的（R3：各自按自己的回退记录分）。
pub fn checkpoints(conn: &dyn Rows, session: &SessionId) -> Result<Checkpoints, StoreError> {
    let mut out = Checkpoints::default();
    for kind in [Kind::Node, Kind::Remote] {
        let rewound: Vec<String> = conn
            .entries(session.as_str(), Some(&kind.file_rewind()))?
            .into_iter()
            .filter(|entry| entry.data["payload"]["reason"] == "file_summary_rewind")
            .filter_map(|entry| {
                entry.data["payload"]["targetMessageId"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        for stored in stored(conn, session.as_str(), kind)? {
            let Some(checkpoint) = stored.kernel else {
                continue;
            };
            if rewound.contains(&stored.prompt) {
                out.reverted.push(checkpoint);
            } else {
                out.active.push(checkpoint);
            }
        }
    }
    Ok(out)
}
