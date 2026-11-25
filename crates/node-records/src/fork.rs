//! Node `/fork` 的新会话（docs/specs/rust-cli/session-lifecycle.md §6.5.2、node-store.md §4.8）：会话行、模型与
//! 模式、克隆的消息（`fork_clone.rs`）、目标状态、时间线与分叉提示在一个 `begin immediate` 事务里写完。
//!
//! 依据：Node `session-fork.ts`（`buildForkedSessionInput`、`createForkedSession`、`forkConversationFromMessage`）、
//! `timeline-persistence.ts`（`persistAssistantTimelinePartForSession`）、`message-persistence.ts`
//! （`persistSyntheticUserNoticeForSession`）与 `helpers/rewind.ts` 的分叉对话提示。
use std::borrow::Cow;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::{Fact, ForkReceipt, SessionFork, StoreError};
use serde_json::{Value, json};

use crate::branch::{Branch, select_branch};
use crate::facts::{self, Commit};
use crate::ids::{message_id, part_id, slugify};
use crate::notices::FORK_NOTICE_SOURCE;
use crate::rows::Rows;
use crate::{records, storage};

/// fork 出的会话的 slug 最多这么多个字符。
const MAX_FORK_SLUG_LEN: usize = 120;
/// fork 出的会话的 `task_type`（Node `buildForkedSessionInput`）。
const FORK_TASK_TYPE: &str = "fork";
pub const CONTINUE: &str = "Continue from this fork. Do not assume messages after the fork point happened in this session.";

/// 新会话行要从原会话行取的列（Node `buildForkedSessionInput`）。
pub(crate) struct Parent {
    pub(crate) project_id: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) trace_id: Option<String>,
    pub(crate) slug: String,
    pub(crate) directory: String,
    pub(crate) path: Option<String>,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) permission: Option<Value>,
    pub(crate) revert: Option<Value>,
}

pub(crate) fn parent(conn: &dyn Rows, id: &str) -> Result<Parent, StoreError> {
    let json = |text: Option<String>| text.and_then(|text| serde_json::from_str(&text).ok());
    let row = conn.fork_source(id)?.ok_or_else(|| StoreError::NotFound {
        session: rust_cli_kernel_api::ids::SessionId::new(id),
    })?;
    Ok(Parent {
        project_id: row.project_id,
        workspace_id: row.workspace_id.filter(|s| !s.is_empty()),
        trace_id: row.trace_id.filter(|s| !s.is_empty()),
        slug: row.slug,
        directory: row.directory,
        path: row.path,
        title: row.title,
        version: row.version,
        permission: json(row.permission),
        revert: json(row.revert),
    })
}

/// JS `String.prototype.slice(0, n)`（slug 只有 ASCII）。
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Node `sanitizeSystemReminderBody`：中和正文里嵌套的 `<system-reminder>` 标签。
fn sanitize(text: &str) -> String {
    text.replace("</system-reminder", "&lt;/system-reminder")
        .replace("<system-reminder", "&lt;system-reminder")
}

/// 分叉提示的正文（Node `formatConversationForkNoticeBody`）。
fn notice_text(fork: &SessionFork) -> String {
    let lines = [
        "This session was forked from a previous session message.".to_owned(),
        format!("parentSessionId: {}", fork.parent),
        format!("targetMessageId: {}", fork.through),
        "No workspace checkpoint was restored for this fork.".to_owned(),
        CONTINUE.to_owned(),
    ];
    sanitize(&lines.join("\n"))
}

/// `forkContext`（提示的消息与 part 的 metadata）与时间线共用的字段；分叉对话不恢复文件，`restoredFileCount`
/// 总是 0（Node 同样写 0）。
fn context(fork: &SessionFork) -> Value {
    json!({"kind": "session_fork", "parentSessionId": fork.parent.as_str(),
        "targetMessageId": fork.through.as_str(), "restoredFileCount": 0})
}

/// Node `persistAssistantTimelinePartForSession`：`session_fork` 分割线。
fn timeline(
    ctx: &Commit<'_>,
    fork: &SessionFork,
    directory: &str,
    anchor: Option<&str>,
) -> Result<(), StoreError> {
    let selection = facts::node_selection(&fork.selection);
    let id = crate::timeline::save(ctx, directory, Some(&selection), anchor)?;
    let now = ctx.now;
    let name = format!("fork_{}_{}", fork.parent, fork.through);
    let mut part = json!({"timelineType": "session_fork", "display": "separator",
        "status": "completed"});
    if let Some(anchor) = anchor {
        part["anchorMessageId"] = anchor.into();
    }
    part["parentSessionId"] = fork.parent.as_str().into();
    part["targetMessageId"] = fork.through.as_str().into();
    part["restoredFileCount"] = 0.into();
    part["time"] = json!({"start": now, "end": now});
    part["id"] = format!("part_{name}_timeline").into();
    part["sessionID"] = ctx.session.into();
    part["messageID"] = id.into();
    part["type"] = "timeline".into();
    ctx.conn.save_part(&part, now as i64)
}

/// Node `persistSyntheticUserNoticeForSession(source = fork)`。
fn notice(ctx: &Commit<'_>, fork: &SessionFork) -> Result<(), StoreError> {
    let id = message_id(ctx.now, &(ctx.uuid)());
    let fork_context = context(fork);
    let info = json!({"id": id, "sessionID": ctx.session, "role": "user",
        "time": {"created": ctx.now}, "agent": records::AGENT,
        "metadata": {"forkContext": fork_context, "source": FORK_NOTICE_SOURCE, "visibility": "model-only"},
        "modelSelection": facts::node_selection(&fork.selection),
        "semantics": {"origin": "agent_runtime", "kind": "fork_notice", "source": FORK_NOTICE_SOURCE,
            "uiVisibility": "hidden", "providerVisibility": "visible", "transcriptVisibility": "hidden"},
        "anchor": {"origin": "synthetic"}, "source": FORK_NOTICE_SOURCE, "synthetic": true, "tools": {},
        "visibility": "model-only"});
    ctx.conn.save_message(&info, ctx.now as i64)?;
    let part = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "text", "text": notice_text(fork), "synthetic": true,
        "time": {"start": ctx.now, "end": ctx.now},
        "metadata": {"forkContext": fork_context, "runtimeMessage": {"source": "rewind_notice"},
            "source": FORK_NOTICE_SOURCE, "visibility": "model-only"}});
    ctx.conn.save_part(&part, ctx.now as i64)
}

/// 调用方给的新会话 id 已经有行时（session-lifecycle.md §10.57、§10.58）：是 `parent` 分出的同一种会话（`task_type`）→ `Existing`；别的会话 → `Exists`。
/// 没有行 → None。删除了的也算已有（id 不复用）。
///
/// 修 bug（§10.57）：新会话的 id 由调用方给（v5 的 `turn.fork`、侧聊）之后，它可能已经有行：重发同一次分叉，或 client 给错了 id。
/// 建会话行（Node `createSession`）是 upsert，以前会改写那个会话的行、把复制的消息追加进它。所以在同一个事务里先查。
pub(crate) fn existing(
    conn: &dyn Rows,
    (parent, child): (&SessionId, &SessionId),
    task_type: &str,
) -> Result<Option<ForkReceipt>, StoreError> {
    let Some(existing) = conn.session(child.as_str())? else {
        return Ok(None);
    };
    if existing.parent_id.as_deref() == Some(parent.as_str()) && existing.task_type == task_type {
        Ok(Some(ForkReceipt::Existing))
    } else {
        Err(StoreError::Exists {
            session: child.clone(),
        })
    }
}

/// fork 与侧聊的新会话行（Node `buildForkedSessionInput`）：项目、工作区、目录、路径、版本、权限与链路取父会话行；
/// `slug = (slugify(原 slug) + "-<task_type>-" + 毫秒的 36 进制).slice(0, 120)`；标题来源 generated。
pub(crate) fn child_row(
    parent: &Parent,
    (parent_id, child): (&SessionId, &SessionId),
    (task_type, title): (&str, String),
    created_at_ms: u64,
) -> crate::rows::Create {
    crate::rows::Create {
        id: child.as_str().to_owned(),
        project_id: parent.project_id.clone(),
        workspace_id: parent.workspace_id.clone(),
        parent_id: Some(parent_id.as_str().to_owned()),
        trace_id: Some(
            parent
                .trace_id
                .clone()
                .unwrap_or_else(|| parent_id.as_str().to_owned()),
        ),
        task_type: Some(task_type.to_owned()),
        slug: head(
            &format!(
                "{}-{task_type}-{}",
                slugify(&parent.slug),
                rust_cli_base::js::base36(created_at_ms)
            ),
            MAX_FORK_SLUG_LEN,
        ),
        directory: parent.directory.clone(),
        path: parent.path.clone(),
        title,
        title_source: Some("generated".to_owned()),
        version: parent.version.clone(),
        permission: parent.permission.clone(),
        time_created: Some(i64::try_from(created_at_ms).unwrap_or(i64::MAX)),
    }
}

/// `/fork` 的新会话（session-lifecycle.md §6.5）；调用方负责事务（本机 `begin immediate`）。
pub fn fork(
    tx: &dyn Rows,
    fork: &SessionFork,
    (now, uuid): (u64, &dyn Fn() -> String),
) -> Result<ForkReceipt, StoreError> {
    if let Some(receipt) = existing(tx, (&fork.parent, &fork.child), FORK_TASK_TYPE)? {
        return Ok(receipt);
    }
    let parent = parent(tx, fork.parent.as_str())?;
    let created = i64::try_from(fork.created_at_ms).unwrap_or(i64::MAX);
    let title = if parent.title.is_empty() {
        rust_cli_kernel_api::title::UNTITLED
    } else {
        parent.title.as_str()
    };
    let input = child_row(
        &parent,
        (&fork.parent, &fork.child),
        (FORK_TASK_TYPE, format!("Fork of {title}")),
        fork.created_at_ms,
    );
    tx.create_session(&input, created)?;
    let ctx = Commit::new(tx, fork.child.as_str(), (now, uuid), None, &[], None);
    facts::apply(
        &ctx,
        &Fact::ModelSelected {
            selection: fork.selection.clone(),
        },
    )?;
    let state = rust_cli_kernel_api::permission::ExecutionState::from_parts(fork.mode, fork.plan);
    facts::apply(&ctx, &Fact::ExecutionChanged { state })?;
    // fork 源分支：只应用 `session.revert`，不按压缩边界截断（Node `forkSourceMessagesForSession`）。
    let records = tx.messages(fork.parent.as_str())?;
    let branch = select_branch(
        records.iter().map(Cow::Borrowed).collect(),
        &Branch::from_revert(parent.revert.as_ref()),
    );
    let end = branch
        .iter()
        .position(|record| record.id() == fork.through.as_str())
        .ok_or_else(|| {
            storage(format!(
                "Checkpoint message not found in session store: {}",
                fork.through
            ))
        })?
        + 1;
    let copied: Vec<&crate::row::WithParts> = branch[..end].iter().map(AsRef::as_ref).collect();
    let map = crate::fork_clone::copy_messages(&ctx, &copied, crate::fork_clone::Copied::AsIs)?;
    crate::fork_clone::copy_goal(&ctx, fork.parent.as_str(), &map)?;
    crate::fork_clone::copy_records(&ctx, fork.parent.as_str(), &map, &copied)?;
    let anchor = map.get(fork.through.as_str()).map(String::as_str);
    timeline(&ctx, fork, &parent.directory, anchor)?;
    notice(&ctx, fork)?;
    facts::next_sequence(&ctx)?;
    Ok(ForkReceipt::Created { copied: end })
}
