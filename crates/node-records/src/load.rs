//! 冷读取：会话行、活动分支上的消息、模型选择与模式；会话列表
//! （docs/specs/rust-cli/node-store.md §5，docs/specs/rust-m11-node-storage.md §6）。
use std::borrow::Cow;
use std::collections::HashMap;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, TraceId, TurnId};
use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::model::{ModelConnection, ModelSelection};
use rust_cli_kernel_api::permission::ExecutionState;
use rust_cli_kernel_api::store::{
    LoadedSession, SessionKind, SessionMeta, SessionSummary, TurnAnchor, TurnRecord, entry_kinds,
};
use rust_cli_kernel_api::store::{SessionFilter, StoreError};
use serde_json::Value;

use crate::LOCAL_ENVIRONMENT;
use crate::branch::{Branch, active_messages, earlier_messages, invalidated};
use crate::facts::live_session;
use crate::hydrate::{Piece, assemble, piece};
use crate::keys;
use crate::mode;
use crate::raw::RawRecord;
use crate::row::WithParts;
use crate::rows::Rows;
use crate::rows::SessionRow;
use crate::state;

/// 冷读取的会话；`checkpoints` 与 `reverted` 留空，由调用方按 [`crate::rewind::Active`] 补出（Node 写的要读产物目录，不在
/// 数据库线程里做，修复线 F12）。
pub fn load(
    conn: &dyn Rows,
    session: &SessionId,
) -> Result<(LoadedSession, crate::rewind::Checkpoints), StoreError> {
    let row = live_session(conn, session)?;
    let raw = conn.messages_raw(session.as_str())?;
    // 分支选择在骨架上做（只看消息与 compaction part，node-store.md §5.4）：整个会话的 part 不同时解析成 JSON
    let skeleton = raw
        .iter()
        .map(RawRecord::skeleton)
        .collect::<serde_json::Result<Vec<_>>>()
        .map_err(crate::storage)?;
    let branch = Branch::from_revert(row.revert.as_ref());
    let active = active_messages(&skeleton, &branch, true);
    // 压缩点之前的历史只供显示（工作线 F102，compact.md §5.1）；模型侧的历史仍是 `active`，单独转换，结果不变。
    let earlier = earlier_messages(&skeleton, &branch, &active);
    let streamed = stream(raw, &active, &earlier)?;
    let loaded = LoadedSession {
        turns: turn_records(conn, session)?,
        inputs: input_records(conn, session)?,
        sources: sources(&skeleton),
        output_heads: streamed.heads,
        tool_times: streamed.times,
        messages: streamed.active,
        earlier: streamed.earlier,
        ..session_level(conn, session, &row)?
    };
    Ok((loaded, crate::rewind::checkpoints(conn, session)?))
}

/// 会话级的读（[`load`] 与范围读同一份，node-store.md §5.6）：会话行的 meta、模型、模式、当前轮、上一轮失败、计划；消息与按消息的事实为空。
pub(crate) fn session_level(
    conn: &dyn Rows,
    session: &SessionId,
    row: &SessionRow,
) -> Result<LoadedSession, StoreError> {
    let model = state::selection(conn, session.as_str())?
        .as_ref()
        .map(kernel_selection)
        .map(|selection| with_connection(conn, session, selection))
        .transpose()?;
    let execution = execution_state(conn, row)?;
    // 库里的当前轮：会话 actor 打开会话时的起点（node-store.md §4，修复线 F6）。
    let turn = state::current_user(conn, session.as_str())?.map(|(prompt, turn)| TurnAnchor {
        prompt: MessageId::new(prompt),
        turn: turn.map(TurnId::new),
    });
    // ExitPlanMode 拿给用户批准的计划（session-lifecycle.md §10.43）：计划文件之后会被改写，冷读的计划行只能读这一份。
    let plans = crate::rust_entries::kind_entries(conn, session.as_str(), entry_kinds::PLAN)?
        .iter()
        .filter_map(|(id, data)| rust_cli_kernel_api::plan::ShownPlan::from_entry(id, data))
        .collect();
    Ok(LoadedSession {
        meta: meta(row),
        messages: Vec::new(),
        model,
        mode: execution.map(|state| state.mode),
        plan: execution.is_some_and(|state| state.plan),
        checkpoints: Vec::new(),
        // 续写时补 assistant 的判断（session-lifecycle.md §6.3.1）：当前轮的 `turn_usage` 是错误。
        last_turn_failed: match &turn {
            Some(anchor) => conn.turn_failed(session.as_str(), anchor.prompt.as_str())?,
            None => false,
        },
        turn,
        earlier: Vec::new(),
        turns: Vec::new(),
        inputs: Vec::new(),
        sources: Vec::new(),
        output_heads: Vec::new(),
        tool_times: Vec::new(),
        plans,
        reverted: Vec::new(),
    })
}

/// 会话 actor 写的回合记录（session-lifecycle.md §10.1）：冷读也能把每条消息还原到它的回合。
pub(crate) fn turn_records(
    conn: &dyn Rows,
    session: &SessionId,
) -> Result<Vec<TurnRecord>, StoreError> {
    Ok(
        crate::rust_entries::kind_entries(conn, session.as_str(), entry_kinds::TURN)?
            .iter()
            .filter_map(|(id, data)| TurnRecord::from_entry(id, data))
            .collect(),
    )
}

/// 输入的显示事实（session-lifecycle.md §10.14）：冷读的行与活会话带同样的附件。
pub(crate) fn input_records(
    conn: &dyn Rows,
    session: &SessionId,
) -> Result<Vec<InputRecord>, StoreError> {
    Ok(
        crate::rust_entries::kind_entries(conn, session.as_str(), entry_kinds::INPUT)?
            .iter()
            .filter_map(|(id, data)| InputRecord::from_entry(id, data))
            .collect(),
    )
}

/// 按 id 读历史的一段（node-store.md §5.5）：只读给出的消息与它们的 part，按给出的顺序逐条整份解码、换成内核消息（同 [`load`]
/// 的 `piece` 与 `assemble`，所以与冷读取里这些消息的那几条相同）；输出头部与工具起止取这些消息里的 tool part；回合记录取开头在其中的。不选分支：
/// 调用方给的就是它上一次整份冷读里的消息。不看会话删没删（前端已经打开着的会话删除之后照常分页）。
pub fn slice(
    conn: &dyn Rows,
    session: &SessionId,
    records: &[MessageId],
) -> Result<rust_cli_kernel_api::store::HistorySlice, StoreError> {
    let ids: Vec<&str> = records.iter().map(MessageId::as_str).collect();
    let mut raw: HashMap<String, RawRecord> = conn
        .messages_raw_of(session.as_str(), &ids)?
        .into_iter()
        .map(|record| (record.id().to_owned(), record))
        .collect();
    let (mut output_heads, mut tool_times, mut pieces) = (Vec::new(), Vec::new(), Vec::new());
    for id in &ids {
        let Some(record) = raw.remove(*id) else {
            continue;
        };
        let full = record.decode().map_err(crate::storage)?;
        for part in full.parts.iter().filter(|part| part["type"] == "tool") {
            output_heads.extend(crate::tool_parts::head_of(part));
            tool_times.extend(crate::tool_parts::time_of(part));
        }
        pieces.push((*id, piece(&full)));
    }
    let turns = crate::rust_entries::kind_entries(conn, session.as_str(), entry_kinds::TURN)?
        .iter()
        .filter_map(|(id, data)| TurnRecord::from_entry(id, data))
        .filter(|turn| records.contains(&turn.first))
        .collect();
    Ok(rust_cli_kernel_api::store::HistorySlice {
        messages: assemble(pieces),
        turns,
        output_heads,
        tool_times,
    })
}

/// 效果记录（session-lifecycle.md §10.3、§10.31）：全部消息记录（含回退掉的分支）里每条带 `anchor.sourceCommandId` 的与它的命令，按存储顺序。
pub(crate) fn sources(
    records: &[crate::row::WithParts],
) -> Vec<rust_cli_kernel_api::store::InputSource> {
    records
        .iter()
        .filter_map(|record| {
            let command = record.info["anchor"][crate::records::SOURCE_COMMAND_ID].as_str()?;
            Some(rust_cli_kernel_api::store::InputSource {
                message: rust_cli_kernel_api::ids::MessageId::new(record.id()),
                command: rust_cli_kernel_api::ids::CommandId::new(command),
            })
        })
        .collect()
}

/// 逐条解码之后的结果：活动历史与 earlier 的内核消息、调用的输出头部与起止。
struct Streamed {
    active: Vec<rust_cli_kernel_api::message::Message>,
    earlier: Vec<rust_cli_kernel_api::message::Message>,
    heads: Vec<rust_cli_kernel_api::command_output::ToolOutputHead>,
    times: Vec<rust_cli_kernel_api::metering::ToolTime>,
}

/// 一条记录换出的片段与还要用几次（`keptMessageIDs` 有重复时同一条记录在活动历史里出现两次）：最后一次移走，之前的复制。
#[derive(Default)]
struct Wanted {
    uses: usize,
    piece: Option<Piece>,
}

impl Wanted {
    fn take(&mut self) -> Option<Piece> {
        self.uses -= 1;
        if self.uses == 0 {
            self.piece.take()
        } else {
            self.piece.clone()
        }
    }
}

/// 逐条消息按存储顺序整份解码一次（node-store.md §5.4）：取调用的输出头部（session-lifecycle.md §10.34）与开工结束（§10.37）——全部消息记录
/// （含回退掉的分支）里的 tool part，按存储顺序，前端按调用 id 查，回退掉的查不到；本工程与 Node 写的同一个键（node-store.md §4.6 第 7 条）；
/// running / pending 的没有起止——再换出活动历史与 earlier 要的片段，随即丢掉这一条的 JSON。保留段里的行（活动历史里的 `Cow::Owned`）另按
/// 改过的副本换一份。最后按两边的顺序拼。
fn stream(
    raw: Vec<RawRecord>,
    active: &[Cow<'_, WithParts>],
    earlier: &[Cow<'_, WithParts>],
) -> Result<Streamed, StoreError> {
    let mut original: HashMap<&str, Wanted> = HashMap::new();
    let mut preserved: HashMap<&str, Wanted> = HashMap::new();
    for entry in active.iter().chain(earlier) {
        let wanted = match entry {
            Cow::Borrowed(_) => &mut original,
            Cow::Owned(_) => &mut preserved,
        };
        wanted.entry(entry.id()).or_default().uses += 1;
    }
    let (mut heads, mut times) = (Vec::new(), Vec::new());
    for record in raw {
        let id = record.id().to_owned();
        let full = record.decode().map_err(crate::storage)?;
        for part in full.parts.iter().filter(|part| part["type"] == "tool") {
            heads.extend(crate::tool_parts::head_of(part));
            times.extend(crate::tool_parts::time_of(part));
        }
        if let Some(wanted) = original.get_mut(id.as_str()) {
            wanted.piece = Some(piece(&full));
        }
        if let Some(wanted) = preserved.get_mut(id.as_str()) {
            wanted.piece = Some(piece(&invalidated(&full)));
        }
    }
    let mut order = |entries: &[Cow<'_, WithParts>]| {
        let pieces = entries
            .iter()
            .map(|entry| {
                let wanted = match entry {
                    Cow::Borrowed(_) => &mut original,
                    Cow::Owned(_) => &mut preserved,
                };
                // 骨架与原文一一对应：用到的每条记录在逐条解码时都换过片段
                let piece = wanted
                    .get_mut(entry.id())
                    .and_then(Wanted::take)
                    .unwrap_or_default();
                (entry.id(), piece)
            })
            .collect();
        assemble(pieces)
    };
    let active = order(active);
    let earlier = order(earlier);
    Ok(Streamed {
        active,
        earlier,
        heads,
        times,
    })
}

pub fn list(conn: &dyn Rows, filter: &SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
    // 本目录的顶层会话与 fork 出的会话，按活动时间倒序（session-lifecycle.md §6.2）。
    let limit = i64::try_from(filter.limit).unwrap_or(i64::MAX);
    let rows = conn.list_sessions(filter.workspace.root.as_str(), Some(limit))?;
    Ok(rows
        .into_iter()
        .map(|row| SessionSummary {
            id: SessionId::new(row.id),
            title: row.title,
            updated_at_ms: u64::try_from(row.time_updated).unwrap_or(0),
        })
        .collect())
}

/// 会话行的 `task_type` → 内核的会话种类（node-store.md §5 第 8 条；会话事实用同一个换算）。
pub fn kind(task_type: &str) -> SessionKind {
    match task_type {
        "subagent_child" => SessionKind::Subagent,
        "fork" => SessionKind::Fork,
        // 侧聊（session-lifecycle.md §10.58）：以前读成 Fork，前端分不出它、把继承的历史当成行
        crate::side_chat::SIDE_CHAT_TASK_TYPE => SessionKind::SideChat,
        _ => SessionKind::Interactive,
    }
}

pub fn meta(row: &SessionRow) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(row.id.as_str()),
        kind: kind(&row.task_type),
        parent: row.parent_id.as_deref().map(SessionId::new),
        workspace: WorkspaceRef {
            // 远程会话的 workspace_id 是 Node 的远程 identity；本地为 NULL。
            environment: EnvironmentId::new(
                row.workspace_id.as_deref().unwrap_or(LOCAL_ENVIRONMENT),
            ),
            root: EnvPath::new(row.directory.as_str()),
        },
        title: row.title.clone(),
        // Node 会话都写 traceID；缺失时（极旧的会话）链路 ID 取会话 ID。
        trace: TraceId::new(row.trace_id.as_deref().unwrap_or(&row.id)),
        created_at_ms: u64::try_from(row.time_created).unwrap_or(0),
    }
}

/// Node `ModelSelection` → 内核的模型选择。
pub fn kernel_selection(selection: &Value) -> ModelSelection {
    let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
    ModelSelection {
        reasoning: selection["options"]["reasoningLevel"]
            .as_str()
            .map(str::to_owned),
        ..ModelSelection::new(text(&selection["providerId"]), text(&selection["modelId"]))
    }
}

/// 团队项目（工作线 F49，node-store.md §5 第 5 条）：`rust-cli/model_connection` 的 `providerId` / `modelId` 与 Node 的
/// 选择相同才用（Node 或 App 之后换过模型时不套用）；三个 ID 都要是去空白后非空的字符串。
fn with_connection(
    conn: &dyn Rows,
    session: &SessionId,
    mut selection: ModelSelection,
) -> Result<ModelSelection, StoreError> {
    let Some(data) =
        state::last_entry(conn, session.as_str(), &keys::named(keys::MODEL_CONNECTION))?
    else {
        return Ok(selection);
    };
    let matches = data["providerId"] == selection.provider.as_str()
        && data["modelId"] == selection.model.as_str();
    let connection = serde_json::from_value::<ModelConnection>(data["connection"].clone())
        .ok()
        .filter(|connection| {
            [
                &connection.product_id,
                &connection.organization_id,
                &connection.project_id,
            ]
            .iter()
            .all(|id| !rust_cli_base::js::string::trim(id).is_empty())
        });
    if matches {
        selection.connection = connection.map(Box::new);
    }
    Ok(selection)
}

/// Node 执行状态对应的权限模式与 plan 开关（修复线 F20）；`rust-cli/permission_mode` 记的模式与执行状态一致时用它
/// （区分 default 与 dontAsk），Node 续写改过模式后以 Node 为准。
pub fn execution_state(
    conn: &dyn Rows,
    row: &SessionRow,
) -> Result<Option<ExecutionState>, StoreError> {
    let Some(execution) = state::execution(conn, row)? else {
        return Ok(None);
    };
    let named = state::last_entry(conn, &row.id, &keys::named(keys::PERMISSION_MODE))?
        .and_then(|data| data["mode"].as_str().and_then(mode::from_entry_name));
    Ok(Some(mode::read(&execution, named)))
}

/// 新会话行（Node `createSession` 的输入，node-store.md §4）：`project_id` 由存储给出（本机按目录算，云端是绑定的项目），
/// 非本机环境的会话在 `workspace_id` 记环境（Node 的远程会话 identity 也记在这里，冷读取按它还原环境）。
pub fn create_input(meta: &SessionMeta, project_id: String, version: &str) -> crate::rows::Create {
    let directory = meta.workspace.root.as_str();
    let task_type = match meta.kind {
        SessionKind::Interactive => None,
        SessionKind::Subagent => Some("subagent_child"),
        SessionKind::Fork => Some("fork"),
        SessionKind::SideChat => Some(crate::side_chat::SIDE_CHAT_TASK_TYPE),
    };
    // fork 的标题是 `Fork of <原会话标题>`、侧聊的是 `Selection side chat`，来源同 Node `buildForkedSessionInput`：generated。其余会话
    // 标题为空时记为 default；首条真实输入按 Node `titleFromInput` 补上（append）。
    let title_source = if matches!(meta.kind, SessionKind::Fork | SessionKind::SideChat) {
        "generated"
    } else if meta.title.is_empty() {
        "default"
    } else {
        "first_input"
    };
    let environment = meta.workspace.environment.as_str();
    crate::rows::Create {
        id: meta.id.as_str().to_owned(),
        project_id,
        workspace_id: (environment != LOCAL_ENVIRONMENT).then(|| environment.to_owned()),
        parent_id: meta.parent.as_ref().map(|p| p.as_str().to_owned()),
        trace_id: Some(meta.trace.as_str().to_owned()),
        task_type: task_type.map(str::to_owned),
        slug: crate::ids::slugify(meta.id.as_str()),
        directory: directory.to_owned(),
        path: Some(directory.to_owned()),
        title: meta.title.clone(),
        title_source: Some(title_source.to_owned()),
        version: version.to_owned(),
        // Node 写 `config.mode ?? "build"`；内核随后提交的 ModeChanged 会更新它。
        permission: Some(serde_json::json!({"mode": "build"})),
        time_created: Some(i64::try_from(meta.created_at_ms).unwrap_or(i64::MAX)),
    }
}
