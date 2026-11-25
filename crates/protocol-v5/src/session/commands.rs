//! 会话命令（02 §6；protocol-v5.md §15.4）：校验（UUIDv7、目标、参数严格、expect）→ 通道执行的一步；通道的结局 → v5 的结果
//! （内核 `Ack` 与 `Refusal` 的换算表在 §15.4.2）。准入只在内核（`admission::admit`），这里不另判「忙不忙」。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{CommandId, TurnId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandResult as KernelResult, Delivery, EditGuard, InputFate, KernelError,
    Persistence, QueuedAttachments, QueuedEdit, QueuedInput,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::interactions::{INTERACTIONS, Reply};
use super::lane::{LiveOp, Operation, Outcome, Rejection, Ticket};
use super::projection::{Projection, QUEUE, WORKS};
use super::rows::ROWS;
use super::schemas;
use super::side_chat;
use super::state::HELD_DISPOSITIONS;
use super::verify::FrontAnswer;
use crate::commands::field_path;
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandResultAccepted, CommandResultAcceptedEffectsItem,
    CommandResultDuplicate, CommandResultRejected, Expect,
};
use crate::generated::session_queue_v1::{
    CommandQueueApplyConfig, CommandQueueEdit, CommandQueueLockEdit, CommandQueueReorder,
    CommandQueueSendNow, CommandQueueSetAutoDrain,
};
use crate::generated::session_v1::{
    CommandConfigSetFollowupMode, CommandConfigSetFollowupModeFollowupMode, CommandConfigSetMode,
    CommandConfigSetModel, CommandInputSend, CommandInputSendDisposition,
    CommandInteractionResolve, CommandSessionCompact, CommandSessionCreate,
    CommandSessionCreateKind, CommandSessionCreatePersistence, CommandSessionRename,
    CommandTurnStop,
};

/// 本线做了的会话命令（protocol-v5.md §15.4.1）。
pub(crate) mod types {
    pub(crate) const SESSION_CREATE: &str = "session.create";
    pub(crate) const SESSION_RENAME: &str = "session.rename";
    pub(crate) const SESSION_DELETE: &str = "session.delete";
    pub(crate) const INPUT_SEND: &str = "input.send";
    pub(crate) const TURN_STOP: &str = "turn.stop";
    pub(crate) const SET_MODEL: &str = "config.setModel";
    pub(crate) const SET_MODE: &str = "config.setMode";
    pub(crate) const SET_FOLLOWUP_MODE: &str = "config.setFollowupMode";
    pub(crate) const QUEUE_DELETE: &str = "queue.delete";
    pub(crate) const QUEUE_AUTO_DRAIN: &str = "queue.setAutoDrain";
    pub(crate) const QUEUE_SEND_NOW: &str = "queue.sendNow";
    pub(crate) const QUEUE_EDIT: &str = "queue.edit";
    pub(crate) const QUEUE_REORDER: &str = "queue.reorder";
    /// 编辑锁与套用配置（protocol-v5.md §64.1）。
    pub(crate) const QUEUE_LOCK_EDIT: &str = "queue.lockEdit";
    pub(crate) const QUEUE_UNLOCK_EDIT: &str = "queue.unlockEdit";
    pub(crate) const QUEUE_APPLY_CONFIG: &str = "queue.applyConfig";
    pub(crate) const SESSION_COMPACT: &str = "session.compact";
    pub(crate) const INTERACTION_RESOLVE: &str = "interaction.resolve";
    pub(crate) const WORK_CANCEL: &str = "work.cancel";
    /// `session.rewind@1`（protocol-v5.md §53）。
    pub(crate) const TURN_EDIT: &str = "turn.edit";
    pub(crate) const TURN_RETRY: &str = "turn.retry";
    pub(crate) const FILES_REWIND: &str = "files.rewind";
    /// `session.fork@1`（protocol-v5.md §55）。
    pub(crate) const TURN_FORK: &str = "turn.fork";
    pub(crate) const ROW_REPORT_HIGHSPEED: &str = "row.reportHighspeed";
}

/// `precondition.heldQueueChanged` 的 `current` 里现在扣住的队列项（同命令参数 `expectedHeldQueueItemIds` 的 id）。
const HELD_QUEUE_ITEM_IDS: &str = "heldQueueItemIds";

/// `queue.reorder` 的锚点参数（拒绝时的 `field`）。
const BEFORE_ID: &str = "beforeId";

/// `queue.edit` 里 `input.send` 没有的键（换算时去掉，§64.1）。
const EDITOR_ID: &str = "editorId";
/// `queue.applyConfig` 里 `input.send` 没有的键。
const QUEUE_ITEM_IDS: &str = "queueItemIds";

/// App 的旧写法：模式 `plan` = plan 开、`build`（同 App 与会话列表的模式名）。
const PLAN_MODE: &str = "plan";

/// 带模式名的参数（`config.setMode` 与 `input.send` 的 `mode`；不认识时拒绝的 `field`）。
pub(super) const MODE_FIELD: &str = "mode";

/// 这条命令属于会话领域（`session`、`session.queue` 两项能力的 schema 里有它）。
pub(crate) fn is_session_command(kind: &str) -> bool {
    schemas::command(kind).is_some()
}

/// 校验通过的会话命令：落在哪个会话、交给通道的哪一步。
pub(crate) struct Accepted {
    pub(crate) key: String,
    pub(crate) sid: String,
    pub(crate) op: Operation,
}

/// 校验没过的命令的结果（装箱：结果不小，函数的 Result 里放指针）。
pub(crate) type Refused = Box<CommandResult>;

pub(crate) fn rejected(code: &str, field: Option<String>, message: Option<String>) -> Refused {
    Box::new(CommandResult::Rejected(CommandResultRejected {
        code: code.to_owned(),
        current: None,
        allowed: None,
        field,
        message,
    }))
}

pub(super) fn invalid(field: &str, message: impl Into<String>) -> Refused {
    rejected(
        codes::PRECONDITION_INVALID_PAYLOAD,
        Some(field.to_owned()),
        Some(message.into()),
    )
}

/// 参数严格（11 §5）：先按 schema 校验（带字段路径），再读成生成的严格类型。
pub(super) fn payload<T: DeserializeOwned>(kind: &str, payload: &Value) -> Result<T, Refused> {
    if let Some(schema) = schemas::command(kind)
        && let Err(error) = schema.validate(payload, "")
    {
        let (field, message) = field_path(&error);
        return Err(invalid(&field, message));
    }
    serde_json::from_value(payload.clone()).map_err(|error| invalid("", error.to_string()))
}

/// topic 里一个实体现在的值（`precondition.versionMismatch` 的 `current{v, data}`，02 §3）。
fn current(publisher: Option<&crate::publisher::Publisher>, col: &str, id: &str) -> Option<Value> {
    let publisher = publisher?;
    let v = publisher.version(col, id)?;
    let mut data = publisher.entity(col, id)?.clone();
    data.insert("id".to_owned(), json!(id));
    Some(json!({"v": v, "data": data}))
}

fn version_mismatch(current: Option<Value>) -> CommandResult {
    CommandResult::Rejected(CommandResultRejected {
        code: codes::PRECONDITION_VERSION_MISMATCH.to_owned(),
        current,
        allowed: None,
        field: None,
        message: None,
    })
}

/// 带 expect 的命令的目标（02 §3）：集合要对、项要在、版本要相符。
pub(super) fn target(
    expect: Option<&Expect>,
    col: &str,
    publisher: Option<&crate::publisher::Publisher>,
    missing: &str,
) -> Result<String, Refused> {
    let Some(Expect::Alt1(expect)) = expect else {
        return Err(invalid(
            "expect",
            format!("this command needs expect{{col: {col}, id, v}}"),
        ));
    };
    if expect.col != col {
        return Err(invalid("expect.col", format!("expect.col must be {col}")));
    }
    match publisher.and_then(|publisher| publisher.version(col, &expect.id)) {
        None => Err(rejected(missing, None, None)),
        Some(v) if v == expect.v => Ok(expect.id.clone()),
        Some(_) => Err(Box::new(version_mismatch(current(
            publisher, col, &expect.id,
        )))),
    }
}

/// `queue.edit` 的目标（protocol-v5.md §17.1、§23.2）：`v` 相符之后，投影里与这个 `v` 对应的整项交给内核再比一次——投影可能落后于
/// 内核（两条命令背靠背、与出队并发），只有内核知道这一项现在是什么。位置不是项的字段（13 Q24）。
fn queue_target<'a>(
    envelope: &CommandEnvelope,
    projection: Option<&'a Projection>,
    publisher: Option<&crate::publisher::Publisher>,
) -> Result<(String, &'a QueuedInput), Refused> {
    let item = target(
        envelope.expect.as_ref(),
        QUEUE,
        publisher,
        codes::PRECONDITION_NOT_FOUND,
    )?;
    let Some(queued) = projection.and_then(|p| p.queued_item(&item)) else {
        return Err(rejected(codes::PRECONDITION_NOT_FOUND, None, None));
    };
    Ok((item, queued))
}

/// `row.reportHighspeed` 的参数：`metrics` 已按命令的 schema 严格校验过（契约 `HighspeedTurnMetrics`），原样成注记的补丁（§46.2）。
#[derive(serde::Deserialize)]
struct Report {
    metrics: serde_json::Map<String, Value>,
}

/// 不比 `v` 的命令按 `expect{col, id}` 指名（契约的 `entityNoVersion`，02 §11.8）：集合要对，返回指名的 id；在不在由调用方按各自的集合判。
fn unversioned(envelope: &CommandEnvelope, col: &str) -> Result<String, Refused> {
    let Some(Expect::Alt1(expect)) = envelope.expect.as_ref() else {
        return Err(invalid(
            "expect",
            format!("this command needs expect{{col: {col}, id, v}}"),
        ));
    };
    if expect.col != col {
        return Err(invalid("expect.col", format!("expect.col must be {col}")));
    }
    Ok(expect.id.clone())
}

/// `queue.reorder` 的目标（§23.2）：只改队列这一级的顺序，不比 `v`。
/// 修 bug（13 Q24）：以前比 `v`，而 `v` 含位置，前一项出队之后的拖动被拒。
fn reorder_target(
    envelope: &CommandEnvelope,
    projection: Option<&Projection>,
) -> Result<String, Refused> {
    let id = unversioned(envelope, QUEUE)?;
    match projection.and_then(|p| p.queued_item(&id)) {
        Some(_) => Ok(id),
        None => Err(rejected(codes::PRECONDITION_NOT_FOUND, None, None)),
    }
}

/// App 的模式名 → 内核的权限模式与 plan 开关。
pub(super) fn mode(name: &str) -> Option<(PermissionMode, bool)> {
    if name == PLAN_MODE {
        return Some((PermissionMode::Default, true));
    }
    Some((PermissionMode::from_node_name(name)?, false))
}

/// 一条会话命令的校验（§15.4.1 的顺序）。`projection` / `publisher` 是这个会话已物化的投影与 topic（判 expect 与交互）。
/// 外壳的前两步（§15.4.1）：commandId 是 UUIDv7、`target.workspaceKey` 是本机的绝对路径。
pub(crate) fn workspace_key(envelope: &CommandEnvelope) -> Result<String, Refused> {
    if !schemas::is_command_id(&envelope.command_id) {
        return Err(rejected(codes::PRECONDITION_INVALID_COMMAND_ID, None, None));
    }
    let Some(key) = envelope.target.workspace_key.clone() else {
        return Err(rejected(
            codes::PRECONDITION_WORKSPACE_REQUIRED,
            Some("target.workspaceKey".to_owned()),
            None,
        ));
    };
    crate::topics::local_workspace(&key).map_err(|message| {
        rejected(
            codes::PRECONDITION_INVALID,
            Some("target.workspaceKey".to_owned()),
            Some(message),
        )
    })?;
    Ok(key)
}

/// 会话级命令必须指名会话（§15.4.1）。
pub(crate) fn session_id(envelope: &CommandEnvelope) -> Result<String, Refused> {
    envelope
        .target
        .session_id
        .clone()
        .filter(|sid| !sid.is_empty())
        .ok_or_else(|| {
            invalid(
                "target.sessionId",
                "a session command needs target.sessionId",
            )
        })
}

/// 落在前端持有的校验上的 `interaction.resolve`（protocol-v5.md §35.2）：外壳照 §15.4.1 的顺序校验，应答按 `requestVerification`
/// 的严格 schema；按 id 认，不比 `v`。不是这一种时为 None（照常 [`accept`]）。
pub(crate) fn front_answer(
    envelope: &CommandEnvelope,
    projection: Option<&Projection>,
) -> Option<Result<FrontAnswer, Refused>> {
    let Some(Expect::Alt1(expect)) = &envelope.expect else {
        return None;
    };
    let front = envelope.r#type == types::INTERACTION_RESOLVE
        && expect.col == INTERACTIONS
        && projection.is_some_and(|projection| projection.verifying(&expect.id));
    front.then(|| {
        let key = workspace_key(envelope)?;
        let sid = session_id(envelope)?;
        let resolve: CommandInteractionResolve =
            payload(types::INTERACTION_RESOLVE, &envelope.payload)?;
        let answer = super::verify::decode(&resolve.answer)
            .map_err(|bad| invalid(&bad.field, bad.message))?;
        Ok(FrontAnswer {
            key,
            sid,
            id: expect.id.clone(),
            answer,
        })
    })
}

pub(crate) fn accept(
    envelope: &CommandEnvelope,
    projection: Option<&Projection>,
    publisher: Option<&crate::publisher::Publisher>,
) -> Result<Accepted, Refused> {
    let key = workspace_key(envelope)?;
    let kind = envelope.r#type.as_str();
    if kind == types::SESSION_CREATE {
        let create: CommandSessionCreate = payload(kind, &envelope.payload)?;
        if create.kind == CommandSessionCreateKind::SideChat {
            return side_chat::accept(key, create);
        }
        if create.kind != CommandSessionCreateKind::Main
            || create.imported_history.is_some()
            || create.first_input.is_some()
        {
            return Err(rejected(
                codes::PRECONDITION_CAPABILITY_UNSUPPORTED,
                None,
                Some("only a main session without first input or imported history".to_owned()),
            ));
        }
        let persistence = match create.persistence {
            Some(CommandSessionCreatePersistence::Immediate) => Persistence::Immediate,
            _ => Persistence::Deferred,
        };
        return Ok(Accepted {
            key,
            sid: create.session_id,
            op: Operation::Create {
                persistence,
                // 所属 Host 用项目的其余 roots 填上（02 §11.9 第 2 项）；以前接受后丢弃（protocol-v5.md §32）。
                directories: create
                    .execution_directories
                    .into_iter()
                    .map(rust_cli_kernel_api::env::EnvPath::new)
                    .collect(),
            },
        });
    }
    let sid = session_id(envelope)?;
    if projection.is_some_and(Projection::deleted) {
        return Err(rejected(codes::ADMISSION_SESSION_DELETED, None, None));
    }
    let op = match kind {
        types::INPUT_SEND => live(input(payload(kind, &envelope.payload)?)?),
        types::TURN_STOP => {
            let stop: CommandTurnStop = payload(kind, &envelope.payload)?;
            live(Command::Interrupt {
                target: Some(TurnId::new(stop.turn_id)),
            })
        }
        types::SET_MODEL => {
            let chosen: CommandConfigSetModel = payload(kind, &envelope.payload)?;
            Operation::Live(LiveOp::SetModel(ModelSelection {
                reasoning: chosen.thought.filter(|thought| !thought.is_empty()),
                ..ModelSelection::new(chosen.provider, chosen.model)
            }))
        }
        types::SET_MODE => {
            let chosen: CommandConfigSetMode = payload(kind, &envelope.payload)?;
            let Some((mode, plan)) = mode(&chosen.mode) else {
                return Err(rejected(
                    codes::PRECONDITION_INVALID_MODE,
                    Some(MODE_FIELD.to_owned()),
                    None,
                ));
            };
            Operation::Live(LiveOp::SetMode { mode, plan })
        }
        // 会话的跟随方式（K-S4，§21）：设值语义，通道按视图比较，值没变是 noop（同 config.setModel）
        types::SET_FOLLOWUP_MODE => {
            let chosen: CommandConfigSetFollowupMode = payload(kind, &envelope.payload)?;
            Operation::Live(LiveOp::SetFollowUp(match chosen.followup_mode {
                CommandConfigSetFollowupModeFollowupMode::Queue => Delivery::Queue,
                CommandConfigSetFollowupModeFollowupMode::Guide => Delivery::Steer,
            }))
        }
        types::SESSION_RENAME => {
            let rename: CommandSessionRename = payload(kind, &envelope.payload)?;
            Operation::Rename(rename.title)
        }
        types::SESSION_DELETE => {
            payload::<Value>(kind, &envelope.payload)?;
            Operation::Delete
        }
        types::QUEUE_DELETE => {
            payload::<Value>(kind, &envelope.payload)?;
            let item = target(
                envelope.expect.as_ref(),
                QUEUE,
                publisher,
                codes::PRECONDITION_NOT_FOUND,
            )?;
            live(Command::Dequeue {
                command: CommandId::new(item),
            })
        }
        types::QUEUE_AUTO_DRAIN => {
            let drain: CommandQueueSetAutoDrain = payload(kind, &envelope.payload)?;
            live(Command::SetAutoDrain {
                enabled: drain.auto_drain,
            })
        }
        types::QUEUE_SEND_NOW => {
            let now: CommandQueueSendNow = payload(kind, &envelope.payload)?;
            let item = target(
                envelope.expect.as_ref(),
                QUEUE,
                publisher,
                codes::PRECONDITION_NOT_FOUND,
            )?;
            live(Command::Promote {
                command: CommandId::new(item),
                target: now.turn_id.map(TurnId::new),
            })
        }
        // 压缩（§17.3）：忙或队列扣住时内核把它排进队列；已有压缩时内核拒绝。草稿没有可压缩的消息，而压缩失败会写一条命令记录、
        // 让放弃的草稿落库，所以在这里拒绝
        // 压缩用这一次提交的模型（§23.2，照 v4）：开跑时内核先把它写成会话的模型
        types::SESSION_COMPACT => {
            let compact: CommandSessionCompact = payload(kind, &envelope.payload)?;
            if projection.is_some_and(Projection::draft) {
                return Err(rejected(codes::ADMISSION_DRAFT, None, None));
            }
            live(Command::CompactOrQueue {
                selection: compact
                    .model_selection
                    .as_ref()
                    .map(|chosen| Box::new(super::inputs::selection(chosen))),
            })
        }
        types::QUEUE_EDIT => queue_edit(envelope, projection, publisher)?,
        // 编辑锁（§64.1）：项在不在、能不能锁、锁是谁的都由内核判（锁的拥有者是会话 actor）；不比 v
        types::QUEUE_LOCK_EDIT => {
            let lock: CommandQueueLockEdit = payload(kind, &envelope.payload)?;
            live(Command::LockQueued {
                command: CommandId::new(unversioned(envelope, QUEUE)?),
                editor: lock.editor_id,
            })
        }
        types::QUEUE_UNLOCK_EDIT => {
            payload::<Value>(kind, &envelope.payload)?;
            live(Command::UnlockQueued {
                command: CommandId::new(unversioned(envelope, QUEUE)?),
            })
        }
        // 套用配置（§64.1）：只取模型与模式（plan 不动），模式名按 input.send 的换法；逐项跳过由内核判
        types::QUEUE_APPLY_CONFIG => {
            let apply: CommandQueueApplyConfig = payload(kind, &envelope.payload)?;
            let send = super::inputs::as_send(&envelope.payload, &[QUEUE_ITEM_IDS], "")?;
            let config = super::inputs::extras(&send)?.config;
            live(Command::ApplyQueuedConfig {
                commands: apply
                    .queue_item_ids
                    .into_iter()
                    .map(CommandId::new)
                    .collect(),
                config: rust_cli_kernel_api::input_extras::TurnConfig {
                    plan: None,
                    ..config
                },
            })
        }
        types::QUEUE_REORDER => {
            let reorder: CommandQueueReorder = payload(kind, &envelope.payload)?;
            let item = reorder_target(envelope, projection)?;
            if let Some(before) = &reorder.before_id
                && projection.and_then(|p| p.queued_item(before)).is_none()
            {
                return Err(rejected(
                    codes::PRECONDITION_NOT_FOUND,
                    Some(BEFORE_ID.to_owned()),
                    None,
                ));
            }
            live(Command::MoveQueued {
                command: CommandId::new(item),
                before: reorder.before_id.map(CommandId::new),
            })
        }
        types::INTERACTION_RESOLVE => {
            let resolve: CommandInteractionResolve = payload(kind, &envelope.payload)?;
            let entity = target(
                envelope.expect.as_ref(),
                INTERACTIONS,
                publisher,
                codes::PRECONDITION_ALREADY_RESOLVED,
            )?;
            let Some((interaction, request)) = projection.and_then(|p| p.interaction(&entity))
            else {
                return Err(rejected(codes::PRECONDITION_ALREADY_RESOLVED, None, None));
            };
            let mode = projection.map_or(PermissionMode::Default, Projection::mode);
            let reply = super::interactions::answer(request, &resolve.answer, mode)
                .map_err(|bad| invalid(&bad.field, bad.message))?;
            live(match reply {
                Reply::Answer(answer) => Command::ResolveInteraction {
                    interaction: interaction.clone(),
                    answer,
                },
                Reply::SwitchMode(mode) => Command::ResolveSwitchingMode {
                    interaction: interaction.clone(),
                    mode,
                },
            })
        }
        // 停一项在跑的后台工作（§20.3）：与 TUI 的 `/tasks`、模型的 TaskStop 同一条停止路径；结束由内核的事件收口
        types::WORK_CANCEL => {
            payload::<Value>(kind, &envelope.payload)?;
            let work = target(
                envelope.expect.as_ref(),
                WORKS,
                publisher,
                codes::PRECONDITION_NOT_FOUND,
            )?;
            live(Command::StopTask { task: work })
        }
        // 改写历史（§53.2）：行换成消息，内核的 `ReplaceInput` / `Rewind`
        types::TURN_EDIT => super::rewind::edit(envelope, projection, publisher)?,
        types::TURN_RETRY => super::rewind::retry(envelope, projection, publisher)?,
        types::FILES_REWIND => super::rewind::files(envelope, publisher)?,
        // fork（§54.2）：回答行换成它所在回合开头的输入，内核分到调用方给的 id
        types::TURN_FORK => super::fork::fork(envelope, publisher)?,
        // 这一轮的完成统计（§46.2）：不比 `v`（后写胜）；行要在 topic 里、要带高速卡。内核按会话的历史再核对一次
        types::ROW_REPORT_HIGHSPEED => {
            let report: Report = payload(kind, &envelope.payload)?;
            let row = unversioned(envelope, ROWS)?;
            if publisher.and_then(|p| p.version(ROWS, &row)).is_none() {
                return Err(rejected(codes::PRECONDITION_NOT_FOUND, None, None));
            }
            if !projection.is_some_and(|p| p.noted(&row)) {
                return Err(rejected(codes::PRECONDITION_NOT_HIGHSPEED, None, None));
            }
            live(Command::PatchNote {
                message: rust_cli_kernel_api::ids::MessageId::new(row),
                patch: report.metrics,
            })
        }
        // 能力里还没做的命令（C3 的缺省，protocol-v5.md §15.7）
        _ => {
            return Err(rejected(
                codes::PRECONDITION_CAPABILITY_UNSUPPORTED,
                None,
                Some(format!("{kind} is not implemented by this agent")),
            ));
        }
    };
    Ok(Accepted { key, sid, op })
}

pub(super) fn live(command: Command) -> Operation {
    Operation::Live(LiveOp::Kernel(command))
}

/// `queue.edit`（§23.2、§64.1）：给了的字段换掉，没给的保留（字段的换法同 `input.send`）。带 `editorId` 时前置条件是编辑锁、不比 `v`
/// （锁住之后只有锁的主人改这一项；「放行并切模式」会改排队项的模式、让 `v` 变，比 `v` 会把持锁的提交拒掉，02 §11.10 A）；不带时照旧比 `v`。
fn queue_edit(
    envelope: &CommandEnvelope,
    projection: Option<&Projection>,
    publisher: Option<&crate::publisher::Publisher>,
) -> Result<Operation, Refused> {
    let edit: CommandQueueEdit = payload(types::QUEUE_EDIT, &envelope.payload)?;
    let (item, queued, guard) = match edit.editor_id.clone() {
        Some(editor) => {
            let item = reorder_target(envelope, projection)?;
            let queued = projection
                .and_then(|p| p.queued_item(&item))
                .ok_or_else(|| rejected(codes::PRECONDITION_NOT_FOUND, None, None))?;
            (item, queued, EditGuard::Lock { editor })
        }
        None => {
            let (item, queued) = queue_target(envelope, projection, publisher)?;
            let guard = EditGuard::Seen {
                expected: Box::new(queued.clone()),
            };
            (item, queued, guard)
        }
    };
    // 压缩是一次维护，不是输入：改了正文它也不会变成输入（02 §6）
    if queued.compact {
        return Err(rejected(
            codes::ADMISSION_QUEUE_ITEM_NOT_EDITABLE,
            None,
            None,
        ));
    }
    let send = super::inputs::as_send(
        &envelope.payload,
        &[super::rewind::NEW_TEXT, EDITOR_ID],
        &edit.new_text,
    )?;
    let extras = super::inputs::extras(&send)?;
    let replaced = QueuedEdit {
        attachments: edit.attachments.is_some().then(|| QueuedAttachments {
            files: extras.files.clone(),
            uploads: extras.uploads.clone(),
        }),
        quotes: edit.quotes.is_some().then(|| extras.quotes.clone()),
        config: extras.config,
        ..QueuedEdit::text(edit.new_text)
    };
    // 同 input.send 的空输入（C1）：按换完之后的这一项判（附件、引用没给时是排着的那一份）
    let mut after = queued.extras.clone();
    if let Some(attachments) = &replaced.attachments {
        after.files.clone_from(&attachments.files);
        after.uploads.clone_from(&attachments.uploads);
    }
    if let Some(quotes) = &replaced.quotes {
        after.quotes.clone_from(quotes);
    }
    if replaced.text.trim().is_empty() && !after.gives_the_model_something() {
        return Err(rejected(codes::PRECONDITION_INPUT_EMPTY, None, None));
    }
    Ok(live(Command::EditQueued {
        command: CommandId::new(item),
        edit: Box::new(replaced),
        guard,
    }))
}

/// `input.send`（C1，02 §11.2）：附件、引用、页面状态、禁用名单与输入框的配置进内核（§15.4.1、§18.1）；没实现的已知字段接受后
/// 丢弃；丢弃之后没有内容 → `inputEmpty`。侧聊的首条输入用同一个换算（§57.2）。
pub(super) fn input(send: CommandInputSend) -> Result<Command, Refused> {
    let extras = super::inputs::extras(&send)?;
    if send.text.trim().is_empty() && !extras.gives_the_model_something() {
        return Err(rejected(codes::PRECONDITION_INPUT_EMPTY, None, None));
    }
    if !send.context_refs.is_empty() {
        tracing::debug!(
            event = "protocol.input.dropped",
            "dropped input fields this agent does not implement"
        );
    }
    // 确认框打开时看到的扣住的队列（02 §6）：只对两种处置有意义，内核与出队同一步核对
    let expected_held = send
        .expected_held_queue_item_ids
        .map(|ids| ids.into_iter().map(CommandId::new).collect());
    let text = send.text;
    let submit = |delivery: Delivery, target: Option<TurnId>| Command::Submit {
        text: text.clone(),
        extras: extras.clone(),
        delivery,
        target,
    };
    Ok(match send.disposition {
        Some(CommandInputSendDisposition::Guide) => {
            let Some(turn) = send.turn_id else {
                return Err(invalid("turnId", "a guide input names the running turn"));
            };
            submit(Delivery::Steer, Some(TurnId::new(turn)))
        }
        // 对扣住的队列的两种处置（§15.5）：各是一条内核命令，前端看到的扣住的队列由内核在同一步里核对（§10.16）
        Some(CommandInputSendDisposition::KeepQueue) => Command::SubmitKeepingQueue {
            text,
            extras,
            expected_held,
        },
        Some(CommandInputSendDisposition::DiscardQueue) => Command::SubmitDiscardingQueue {
            text,
            extras,
            expected_held,
        },
        // 三种投递各是一条内核命令（§22.0）：以前三种都当排队，设成插话的回车与 Cmd/Ctrl+Enter 的「现在发」都只排队
        Some(CommandInputSendDisposition::Enqueue) => submit(Delivery::Queue, None),
        // 现在发（13 Q17）：回合在跑时内核在同一步里抢占它
        Some(CommandInputSendDisposition::StartNow) => Command::SubmitNow { text, extras },
        // 没给处置：按会话的跟随方式（K-S4），由内核在准入那一刻判
        None => Command::SubmitFollowUp { text, extras },
    })
}

/// 通道的结局 → v5 的结果（§15.4.2）。`Err` 是 JSON-RPC 错误（`internal.failed`：内部错误是 failed，不是 rejected）。
pub(crate) fn finish(
    ticket: &Ticket,
    sid: &str,
    outcome: Outcome,
    projection: Option<&Projection>,
    publisher: Option<&crate::publisher::Publisher>,
    topic: &str,
) -> Result<CommandResult, Rejection> {
    let (kind, command_id) = (ticket.kind.as_str(), ticket.id.as_str());
    // 内核按前端看到的那一份判出「变了」（`Stale`，§17.1）：带上目标实体回执时的值（让它被拒的改动已经在 topic 里，§15.4.2）
    let stale = ticket
        .entity
        .as_deref()
        .and_then(|id| current(publisher, QUEUE, id));
    let effects: Vec<CommandResultAcceptedEffectsItem> = publisher
        .map(crate::publisher::Publisher::cursor)
        .map(|cursor| CommandResultAcceptedEffectsItem {
            topic: topic.to_owned(),
            epoch: cursor.epoch,
            seq: cursor.seq,
        })
        .into_iter()
        .collect();
    let accepted = |noop: bool, result: Option<Value>| {
        CommandResult::Accepted(CommandResultAccepted {
            effects: effects.clone(),
            noop,
            result,
        })
    };
    match outcome {
        Outcome::Ack(ack) if kind == types::SESSION_CREATE => side_chat::done(sid, ack, &accepted),
        Outcome::Ack(ack) => from_ack(kind, command_id, ack, projection, stale, &accepted, None),
        Outcome::Edited { ack, preview } => from_ack(
            kind,
            command_id,
            ack,
            projection,
            stale,
            &accepted,
            preview.as_ref(),
        ),
        Outcome::Created(Ok(())) => Ok(accepted(false, Some(json!({"sessionId": sid})))),
        Outcome::Created(Err(KernelError::Exists { .. })) => {
            Ok(*rejected(codes::PRECONDITION_SESSION_EXISTS, None, None))
        }
        Outcome::Created(Err(error)) => Err(Rejection {
            code: codes::INTERNAL_FAILED,
            message: error.to_string(),
        }),
        Outcome::Noop => Ok(accepted(true, None)),
        Outcome::Renamed(_) | Outcome::Deleted => Ok(accepted(false, None)),
        Outcome::Refused(rejection) if rejection.code == codes::INTERNAL_FAILED => Err(rejection),
        Outcome::Refused(rejection) => Ok(*rejected(rejection.code, None, Some(rejection.message))),
    }
}

/// `preview`：编辑被挡住时（文件恢复不了）那时的预览（§53.2），结果是 `blocked` 而不是拒绝。
fn from_ack(
    kind: &str,
    command_id: &str,
    ack: Ack,
    projection: Option<&Projection>,
    stale: Option<Value>,
    accepted: &dyn Fn(bool, Option<Value>) -> CommandResult,
    preview: Option<&Value>,
) -> Result<CommandResult, Rejection> {
    match ack {
        Ack::Accepted { result } => Ok(accepted_result(
            kind, command_id, result, projection, accepted,
        )),
        Ack::Rejected {
            refusal: Refusal::Unavailable,
            ..
        } if kind == types::TURN_EDIT => Ok(accepted(
            false,
            Some(super::rewind::blocked_result(preview.cloned())),
        )),
        // 重放（02 §6、§11.2 A）：原来的回执按同一张表换算；effects 是现在的位置（G1）
        Ack::Duplicate { original } => {
            let original = from_ack(
                kind, command_id, *original, projection, stale, accepted, preview,
            )?;
            Ok(CommandResult::Duplicate(CommandResultDuplicate {
                original: serde_json::to_value(original).unwrap_or(Value::Null),
            }))
        }
        Ack::Rejected { refusal, reason } => refusal_result(kind, refusal, reason, stale),
        Ack::OutcomeUnknown { reason } => Ok(outcome_unknown(reason)),
    }
}

/// 存储说提交的结果未知（kernel-api 的 `Ack::OutcomeUnknown`，有持久输入账本的会话库才给）：不是确定的拒绝，也不能当成
/// 执行了。照契约里「查不到裁决」的码回（05 §0.1 的 `precondition.operationUnknown`）：由用户决定是否重试，不盲目再执行。
pub(super) fn outcome_unknown(reason: String) -> CommandResult {
    CommandResult::Rejected(CommandResultRejected {
        code: codes::PRECONDITION_OPERATION_UNKNOWN.to_owned(),
        current: None,
        allowed: None,
        field: None,
        message: Some(reason),
    })
}

fn accepted_result(
    kind: &str,
    command_id: &str,
    result: KernelResult,
    projection: Option<&Projection>,
    accepted: &dyn Fn(bool, Option<Value>) -> CommandResult,
) -> CommandResult {
    match (kind, result) {
        (types::TURN_STOP, KernelResult::Halted { queued, cancelled }) => {
            let ids = |ids: Vec<CommandId>| {
                ids.into_iter()
                    .map(|id| id.as_str().to_owned())
                    .collect::<Vec<_>>()
            };
            accepted(
                false,
                Some(json!({"stillQueued": ids(queued), "cancelled": ids(cancelled)})),
            )
        }
        (types::TURN_STOP, KernelResult::Unchanged) => {
            accepted(true, Some(json!({"stillQueued": [], "cancelled": []})))
        }
        // 要的状态已经成立（放开没有的锁、套用之后一项都没变，§64.1）：noop
        (_, KernelResult::Unchanged) => accepted(true, None),
        (types::INPUT_SEND, _) => {
            // 实际的投递（02 §4.2）：取这条输入在回执之前的结局；跨进程的重放（效果记录）没有结局，落进对话的就是开过回合的
            let delivery = match projection.and_then(|p| p.fate(command_id)) {
                Some(InputFate::Queued) => {
                    match projection.and_then(|p| p.queued_delivery(command_id)) {
                        Some(Delivery::Steer) => "guide",
                        _ => "queue",
                    }
                }
                _ => "startNow",
            };
            accepted(
                false,
                Some(json!({"delivery": delivery, "inputId": command_id})),
            )
        }
        // 改写了（§53.2）：跨进程的重放（效果记录）没有原来的结果，落进对话的就是改写了的
        (types::TURN_EDIT, _) => accepted(false, Some(super::rewind::rewound_result())),
        // 分出了（§54.2）：已经是这次分叉的会话时内核回同一个会话，结果相同（不分 noop，同 session.create）
        (types::TURN_FORK, KernelResult::Forked(forked)) => {
            accepted(false, Some(super::fork::result(&forked)))
        }
        _ => accepted(false, None),
    }
}

/// 内核的拒绝 → 02 的码（§15.4.2 的表）。`Failed` 是内部错误。`stale` 是 `Stale` 时目标实体现在的值。
pub(super) fn refusal_result(
    kind: &str,
    refusal: Refusal,
    reason: String,
    stale: Option<Value>,
) -> Result<CommandResult, Rejection> {
    let (code, current, allowed) = match refusal {
        Refusal::Busy => (codes::ADMISSION_BUSY, None, None),
        Refusal::NotRunning => (codes::ADMISSION_NOT_RUNNING, None, None),
        Refusal::StopTargetChanged { current } => (
            codes::ADMISSION_STOP_TARGET_CHANGED,
            Some(json!({"turnId": current.as_str()})),
            None,
        ),
        Refusal::QueueChoiceRequired => (
            codes::ADMISSION_QUEUE_CHOICE_REQUIRED,
            None,
            Some(HELD_DISPOSITIONS.iter().map(|d| (*d).to_owned()).collect()),
        ),
        // 会话正在关：之后再发由通道重开
        Refusal::Closed => (codes::PRECONDITION_NOT_FOUND, None, None),
        Refusal::CommandIdConflict => (codes::PRECONDITION_COMMAND_ID_CONFLICT, None, None),
        // 确认框看到的扣住的队列已经变了（02 §6）：带上现在扣住的，前端据此重新问
        Refusal::HeldQueueChanged { current } => (
            codes::PRECONDITION_HELD_QUEUE_CHANGED,
            Some(
                json!({ HELD_QUEUE_ITEM_IDS: current.iter().map(CommandId::as_str).collect::<Vec<_>>() }),
            ),
            None,
        ),
        Refusal::NotFound if kind == types::INTERACTION_RESOLVE => {
            (codes::PRECONDITION_ALREADY_RESOLVED, None, None)
        }
        Refusal::NotFound => (codes::PRECONDITION_NOT_FOUND, None, None),
        // 回退文件：没有可恢复的、冲突、读不出（§53.2），用 02 已登记的码
        Refusal::Unavailable if kind == types::FILES_REWIND => {
            (codes::PRECONDITION_REWIND_UNAVAILABLE, None, None)
        }
        // fork、侧聊到的 id 已经是别的会话（§55.2、§57.2，02 §11.2 F）
        Refusal::Unavailable if kind == types::TURN_FORK || kind == types::SESSION_CREATE => {
            (codes::PRECONDITION_SESSION_EXISTS, None, None)
        }
        // 上报的行没有高速卡（内核按它的历史核对，§46.2）
        Refusal::Unavailable if kind == types::ROW_REPORT_HIGHSPEED => {
            (codes::PRECONDITION_NOT_HIGHSPEED, None, None)
        }
        // 锁或改的不是排在回合之后的普通文字（压缩、插话、bash、内容块，§64.1）：同改压缩项的码
        Refusal::Unavailable if kind == types::QUEUE_LOCK_EDIT || kind == types::QUEUE_EDIT => {
            (codes::ADMISSION_QUEUE_ITEM_NOT_EDITABLE, None, None)
        }
        Refusal::Unavailable => (codes::PRECONDITION_INVALID, None, None),
        Refusal::Stale => (codes::PRECONDITION_VERSION_MISMATCH, stale, None),
        Refusal::CompactLocked => (codes::ADMISSION_COMPACT_LOCKED, None, None),
        // 编辑锁（§64.1）
        Refusal::QueueItemLocked => (codes::ADMISSION_QUEUE_ITEM_EDIT_LOCKED, None, None),
        Refusal::EditLockLost => (codes::PRECONDITION_QUEUE_EDIT_LOCK_LOST, None, None),
        Refusal::Failed => {
            return Err(Rejection {
                code: codes::INTERNAL_FAILED,
                message: reason,
            });
        }
    };
    Ok(CommandResult::Rejected(CommandResultRejected {
        code: code.to_owned(),
        current,
        allowed,
        field: None,
        message: Some(reason),
    }))
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "commands_report_tests.rs"]
mod report_tests;
