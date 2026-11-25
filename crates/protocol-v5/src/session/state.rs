//! 会话 topic 的 state 区（02 §1.1；protocol-v5.md §15.3.1）：每个键整体替换（`state.set`），键在快照 state 的顶层。
//! 准入（`admission`）由内核的 `admission::admit` 求出（与会话 actor 同一个函数，02 §4 修订 10）。

use std::collections::BTreeMap;

use rust_cli_kernel_api::admission::{Activity, Admission, AdmissionView, Refusal, admit};
use rust_cli_kernel_api::agents::TaskRecord;
use rust_cli_kernel_api::compaction::ContextOccupancy;
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId, TurnId};
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Command, Delivery, NoticeKind, NoticeLevel, PlanSwitch, QueueHold, RewindScope,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use serde_json::{Value, json};

use super::rows::millis;
use crate::generated::contract::codes;
use crate::generated::session_v1::{
    SessionStateNoticesItem, SessionStateNoticesItemKind, SessionStateNoticesItemLevel,
};

/// state 区的键（`SESSION_STATE_KEY_SCHEMAS`）。
pub(crate) mod keys {
    pub(crate) const CONTROL: &str = "control";
    pub(crate) const META: &str = "meta";
    pub(crate) const CONFIG: &str = "config";
    pub(crate) const USAGE: &str = "usage";
    pub(crate) const GOAL: &str = "goal";
    pub(crate) const PLAN: &str = "plan";
    pub(crate) const SHARED_CONTEXT: &str = "sharedContext";
    pub(crate) const QUEUE_POLICY: &str = "queuePolicy";
    pub(crate) const ADMISSION: &str = "admission";
    pub(crate) const HOOK_ADMISSION: &str = "hookAdmission";
    pub(crate) const MCP_ADMISSION: &str = "mcpAdmission";
    pub(crate) const ROWS_WINDOW: &str = "rowsWindow";
    pub(crate) const QUEUE_ORDER: &str = "queueOrder";
    pub(crate) const NOTICES: &str = "notices";
}

/// `control.phase`（`SESSION_PHASES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Draft,
    Idle,
    Running,
    Completed,
    Error,
    Deleted,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Deleted => "deleted",
        }
    }
}

/// 上一轮的结局（`control.lastOutcome`，`LAST_OUTCOMES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    Interrupted,
    Error,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Interrupted => "interrupted",
            Self::Error => "error",
        }
    }
}

/// 在跑的回合。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Running {
    pub(crate) turn: TurnId,
    pub(crate) compact: bool,
    /// 开这个回合的命令（`TurnStarted.command`）：压缩回合里的标记行带它（protocol-v5.md §24.1）。
    pub(crate) command: Option<rust_cli_kernel_api::ids::CommandId>,
    pub(crate) started_at: u64,
    /// 开始时这个 topic 的 seq（`activeWorks[].startedAtSeq`）。
    pub(crate) seq: u64,
    /// 主循环里在跑的那次压缩在回合记录里的位置（protocol-v5.md §50.2）：边界提交时边界行接过它的行号。
    pub(crate) loop_compaction: Option<usize>,
}

impl Running {
    /// 由回合的开始建（事件 `TurnStarted` 与视图 `running` 里的回合开始是同一份事实，protocol-v5.md §25.1）。
    pub(crate) fn of(start: &rust_cli_kernel_api::session_api::TurnStart, seq: u64) -> Self {
        Self {
            turn: start.turn.clone(),
            compact: start.origin == rust_cli_kernel_api::session_api::TurnOrigin::Compaction,
            command: start.command.clone(),
            started_at: start.started_at,
            seq,
            loop_compaction: None,
        }
    }

    /// 压缩回合的命令（不是压缩回合为 None）：这个回合里的压缩标记带它（protocol-v5.md §24.1）。
    pub(crate) fn compaction(&self) -> Option<&rust_cli_kernel_api::ids::CommandId> {
        self.command.as_ref().filter(|_| self.compact)
    }
}

/// `control` 要的事实。
pub(crate) struct ControlFacts<'a> {
    pub(crate) phase: Phase,
    pub(crate) live: bool,
    pub(crate) running: Option<&'a Running>,
    pub(crate) outcome: Option<Outcome>,
    pub(crate) error: Option<&'a Value>,
    pub(crate) retry: Option<&'a Value>,
}

/// 回合失败的种类（`ModelError` 的 serde 标签）；不是模型错误的失败为 `turn_failed`（protocol-v5.md §15.3.1）。
const TURN_FAILED: &str = "turn_failed";
const ERROR_KIND_KEY: &str = "kind";
/// `lastError.source`。
const SOURCE_PROVIDER: &str = "provider";
const SOURCE_RUNTIME: &str = "runtime";

/// `control.lastError.attribution`（契约 `ErrorAttribution`，§46.1）。
const ATTRIBUTION: &str = "attribution";

/// `control.lastError`：`cause` 的种类作 `code`；可重试的种类（限流、过载、网络、服务端、流空闲）`recoverable`。`attribution` 是这一轮用的卡（§46.1）。
pub(crate) fn last_error(message: &str, cause: Option<&ModelError>, attribution: Value) -> Value {
    let Some(cause) = cause else {
        return json!({"code": TURN_FAILED, "message": message, "source": SOURCE_RUNTIME, "recoverable": false,
            ATTRIBUTION: attribution});
    };
    let code = serde_json::to_value(cause)
        .ok()
        .and_then(|value| {
            value
                .get(ERROR_KIND_KEY)
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| TURN_FAILED.to_owned());
    let recoverable = matches!(
        cause,
        ModelError::RateLimited { .. }
            | ModelError::Overloaded { .. }
            | ModelError::Network { .. }
            | ModelError::Server { .. }
            | ModelError::StreamIdle { .. }
    );
    json!({"code": code, "message": message, "source": SOURCE_PROVIDER, "recoverable": recoverable,
        ATTRIBUTION: attribution})
}

/// UserPromptSubmit 拦下时的 `control.lastError`（protocol-v5.md §49.2，照 v4 的 `hookBlockErrorDelta`）：界面按这个码画 hook 图标。
const HOOK_BLOCKED: &str = "fault.runtime.hookBlocked";

pub(crate) fn hook_blocked(message: &str, detail: &str, at: u64, attribution: Value) -> Value {
    json!({"code": HOOK_BLOCKED, "message": message, "recoverable": false, "at": millis(at),
        "source": SOURCE_RUNTIME, "detail": detail, ATTRIBUTION: attribution})
}

/// `control.apiRetry`：`nextRetryAt = at + delay_ms`（session-lifecycle.md §10.9）。
pub(crate) fn api_retry(
    attempt: u32,
    max: u32,
    error: &ModelError,
    at: u64,
    delay_ms: u64,
) -> Value {
    json!({
        "attempt": attempt,
        "max": max,
        "reason": error.to_string(),
        "nextRetryAt": millis(at.saturating_add(delay_ms)),
    })
}

/// `activeWorks[].kind`。
const WORK_TURN: &str = "turn";
const WORK_COMPACT: &str = "compact";
/// `control.runtime`。
const RUNTIME_COLD: &str = "cold";
const RUNTIME_READY: &str = "ready";
const RUNTIME_BUSY: &str = "busy";

pub(crate) fn control(facts: &ControlFacts<'_>) -> Value {
    let runtime = match (facts.live, facts.running) {
        (false, _) => RUNTIME_COLD,
        (true, Some(_)) => RUNTIME_BUSY,
        (true, None) => RUNTIME_READY,
    };
    let works: Vec<Value> = facts
        .running
        .map(|running| {
            json!({
                "id": running.turn.as_str(),
                "kind": if running.compact { WORK_COMPACT } else { WORK_TURN },
                "startedAtSeq": running.seq,
                "startedAt": millis(running.started_at),
            })
        })
        .into_iter()
        .collect();
    json!({
        "phase": facts.phase.as_str(),
        "runtime": runtime,
        "turnId": facts.running.map(|running| running.turn.as_str()),
        "activeWorks": works,
        "lastError": facts.error,
        "apiRetry": facts.retry,
        "lastOutcome": facts.outcome.map(Outcome::as_str),
    })
}

/// `meta`：`kind` 是侧聊时 `sideChat`（protocol-v5.md §57.5），其余 `main`（子会话的 `child` 不在本线，§15.3.1）。
pub(crate) fn meta(title: &str, source: TitleSource, created_at: u64, side_chat: bool) -> Value {
    let source = match source {
        TitleSource::Default => "default",
        TitleSource::FirstInput => "firstInput",
        TitleSource::Generated => "generated",
        TitleSource::Custom => "custom",
    };
    let kind = if side_chat { KIND_SIDE_CHAT } else { KIND_MAIN };
    json!({"title": title, "titleSource": source, "createdAt": millis(created_at), "kind": kind})
}

/// `meta.kind` 的两种写法（02 §1.1）。
const KIND_MAIN: &str = "main";
const KIND_SIDE_CHAT: &str = "sideChat";

/// 稀疏的模型选择（`config.modelSelection` 与队列项的 `modelSelection` 同形）。
pub(crate) fn model_selection(selection: &ModelSelection) -> Value {
    let mut chosen = json!({"providerId": selection.provider, "modelId": selection.model});
    if let Some(level) = &selection.reasoning {
        chosen["options"] = json!({"reasoningLevel": level});
    }
    chosen
}

/// `config` 里两个给输入框的事实（K-C1-INTENT，protocol-v5.md §18.2）：最近一次「完全访问」的询问、最近一次工具切的 plan。
pub(crate) struct Hints<'a> {
    pub(crate) grant: Option<&'a ToolUseId>,
    pub(crate) plan_switch: Option<&'a PlanSwitch>,
}

/// `config.followupMode`（`FOLLOWUP_MODES`）。
const FOLLOWUP_QUEUE: &str = "queue";
const FOLLOWUP_GUIDE: &str = "guide";

/// `config`：模式用 App 的模式名（`PermissionMode::node_name`，同会话列表的 `config.mode`）；`followupMode` 是内核的会话跟随方式（§22.1）。
/// 没有授权、工具没切过 plan 时不写那两个键（契约里缺席 = 没有）。没有档位列表（22 §15.3 删了 `thoughtLevels`）：界面的档位取 Host 的
/// providers 目录。
pub(crate) fn config(
    selection: &ModelSelection,
    mode: PermissionMode,
    plan: bool,
    follow_up: Delivery,
    hints: &Hints<'_>,
) -> Value {
    let thought = selection.reasoning.clone().unwrap_or_default();
    let mut config = json!({
        "provider": selection.provider,
        "model": selection.model,
        "thought": thought,
        "mode": mode.node_name(),
        "followupMode": match follow_up {
            Delivery::Queue => FOLLOWUP_QUEUE,
            Delivery::Steer => FOLLOWUP_GUIDE,
        },
        "planEnabled": plan,
        "modelSelection": model_selection(selection),
    });
    // 询问实体的 id 就是 tool_use（§15.6），所以这就是那个询问的 id
    if let Some(grant) = hints.grant {
        config["permissionGrant"] = json!({"interactionId": grant.as_str()});
    }
    if let Some(switch) = hints.plan_switch {
        config["planTransition"] =
            json!({"toolCallId": switch.tool_use.as_str(), "planEnabled": switch.enabled});
    }
    config
}

/// `usage`：`contextWindow.max` 是模型的标称窗口，不知道窗口时 `contextWindow: null`（protocol-v5.md §50.3）；`cumulative` 恒 0（K-S8 没做）。
///
/// 修 bug（X2，§50.3）：以前 `max` 是内核判压缩用的有效窗口（窗口减输出的预留，1M 的模型 980000），不知道窗口的模型也画默认窗口；v4 的
/// 上限是配置的窗口（1000000），不知道时不画。用户暂定照 Node；内核判压缩与 TUI 仍用有效窗口（`max_tokens`）。
pub(crate) fn usage(context: Option<&ContextOccupancy>) -> Value {
    let window = context.and_then(|context| {
        let max = context.window?;
        let mut window = json!({"used": context.used_tokens, "max": max});
        if let Some(at) = context.compact_at {
            window["autoCompactThreshold"] = json!(at);
        }
        Some(window)
    });
    json!({
        "contextWindow": window,
        "cumulative": {"inputTokens": 0, "outputTokens": 0, "cacheRead": 0, "cacheWrite": 0},
    })
}

/// `queuePolicy`（02 §1.1、§11 C2）。
pub(crate) fn queue_policy(hold: Option<QueueHold>) -> Value {
    match hold {
        None => json!({"autoDrain": true}),
        Some(QueueHold::Stopped) => json!({"autoDrain": false, "pauseReason": "stopped"}),
        Some(QueueHold::TurnFailed) => json!({"autoDrain": false, "pauseReason": "turnFailed"}),
        Some(QueueHold::Requested) => json!({"autoDrain": false}),
    }
}

/// 没有数据的键的缺省值（goal：M9 暂定不做；sharedContext、hookAdmission：null）。
pub(crate) fn defaults() -> [(&'static str, Value); 3] {
    [
        (
            keys::GOAL,
            json!({"targetId": null, "status": "none", "iteration": 0, "verifications": [], "iterations": []}),
        ),
        (keys::SHARED_CONTEXT, Value::Null),
        (keys::HOOK_ADMISSION, Value::Null),
    ]
}

/// `plan.updatedAt`：清单文件没有修改时刻，用事件到达的时刻会让活会话与冷读不同值，所以恒为契约的缺省（界面不读它，§50.2）。
const PLAN_UPDATED_AT: u64 = 0;

/// `state.plan`（protocol-v5.md §51.2）：任务清单按清单的顺序（ID 升序），每项 `{id, text: subject, status}`，状态同名。
/// 由视图建与按 `TasksChanged` 更新都调它，冷读与活会话同值。
pub(crate) fn plan(tasks: &[TaskRecord]) -> Value {
    let items: Vec<Value> = tasks
        .iter()
        .map(|task| json!({"id": task.id, "text": task.subject, "status": task.status.as_str()}))
        .collect();
    json!({"items": items, "updatedAt": PLAN_UPDATED_AT})
}

/// 扣住队列时可选的处置（02 §4.1 的 choice）。
pub(crate) const HELD_DISPOSITIONS: [&str; 2] = ["keepQueue", "discardQueue"];

fn allow() -> Value {
    json!({"allow": true})
}

fn deny(reason: &str) -> Value {
    json!({"allow": false, "reason": reason})
}

/// 准入要看的投影里的事实（与内核的 `AdmissionView` 对应，另加前端才知道的删除与草稿）。
pub(crate) struct Gates<'a> {
    pub(crate) running: Option<&'a TurnId>,
    /// 扣住且队列非空。
    pub(crate) held: bool,
    /// 队首被编辑锁冻结（§64.1）。
    pub(crate) frozen: bool,
    pub(crate) deleted: bool,
    /// 本进程以 deferred 建、还没有回合的草稿。
    pub(crate) draft: bool,
    /// 有在跑或排队的压缩（§17.3）。
    pub(crate) compaction: bool,
    /// 会话的跟随方式（§22.1）。
    pub(crate) follow_up: Delivery,
    /// 侧聊会话（§57.4）：侧聊不接受的命令的门 deny。
    pub(crate) side_chat: bool,
}

/// `state.admission`（protocol-v5.md §15.4.3）：对一组代表命令求内核的 `admit`。没声明的能力的门一律不允许。
pub(crate) fn admission(gates: &Gates<'_>) -> Value {
    let Gates {
        running,
        held,
        frozen,
        deleted,
        draft,
        compaction,
        follow_up,
        side_chat,
    } = *gates;
    let unsupported = deny(codes::PRECONDITION_CAPABILITY_UNSUPPORTED);
    if deleted {
        let gone = deny(codes::ADMISSION_SESSION_DELETED);
        return json!({
            "input": {"mode": "startNow", "reason": codes::ADMISSION_SESSION_DELETED, "allowed": []},
            "stop": gone, "compact": gone, "fork": gone, "edit": gone, "retry": gone,
            "rewindFiles": gone, "queueSendNow": gone, "goalPause": gone, "goalResume": gone,
        });
    }
    let view = AdmissionView {
        activity: running.map_or(Activity::Idle, |turn| Activity::Turn(turn.clone())),
        closing: false,
        held,
        frozen,
        compaction,
        follow_up,
    };
    // 代表的输入是没给处置的那一种（输入框回车发的就是它，§22.0）：在跑时按跟随方式是插话还是排队
    let submit = Command::SubmitFollowUp {
        text: String::new(),
        extras: Default::default(),
    };
    let input = match admit(&view, &submit) {
        Admission::Enqueue(Delivery::Steer) => {
            json!({"mode": "guide", "reason": codes::ADMISSION_BUSY, "allowed": []})
        }
        // 空闲时排队只因为队首被锁着（§64.1）：原因写它，不写「忙」
        Admission::Enqueue(Delivery::Queue) if running.is_none() => {
            json!({"mode": "enqueue", "reason": codes::ADMISSION_QUEUE_HEAD_EDIT_LOCKED, "allowed": []})
        }
        Admission::Enqueue(Delivery::Queue) => {
            json!({"mode": "enqueue", "reason": codes::ADMISSION_BUSY, "allowed": []})
        }
        Admission::Deny(Refusal::QueueChoiceRequired) => json!({
            "mode": "choice",
            "reason": codes::ADMISSION_QUEUE_CHOICE_REQUIRED,
            "allowed": HELD_DISPOSITIONS,
        }),
        Admission::Run | Admission::Unchanged | Admission::Deny(_) => {
            json!({"mode": "startNow", "allowed": []})
        }
    };
    let stop = match running {
        Some(turn) => {
            let interrupt = Command::Interrupt {
                target: Some(turn.clone()),
            };
            match admit(&view, &interrupt) {
                Admission::Run => allow(),
                _ => deny(codes::ADMISSION_NOT_RUNNING),
            }
        }
        None => deny(codes::ADMISSION_NOT_RUNNING),
    };
    // 压缩（§17.3）：草稿没有可压缩的消息；视图不在关闭，内核的拒绝只有「已有压缩」
    let compact = if draft {
        deny(codes::ADMISSION_DRAFT)
    } else {
        match admit(&view, &Command::CompactOrQueue { selection: None }) {
            Admission::Deny(_) => deny(codes::ADMISSION_COMPACT_LOCKED),
            Admission::Run | Admission::Enqueue(_) | Admission::Unchanged => allow(),
        }
    };
    // 改写历史（§53.4）：编辑与重试是内核的 `ReplaceInput`（回合在跑时抢占它），回退文件是 `Rewind{Code}`（只在空闲时）；目标在命令里，门只看
    // 会话此刻能不能做
    let rewrite = |command: &Command| {
        if draft {
            return deny(codes::ADMISSION_DRAFT);
        }
        match admit(&view, command) {
            Admission::Run | Admission::Enqueue(_) | Admission::Unchanged => allow(),
            Admission::Deny(Refusal::Busy) if compaction => deny(codes::ADMISSION_COMPACTING),
            Admission::Deny(_) => deny(codes::ADMISSION_BUSY),
        }
    };
    let target = MessageId::new(String::new());
    let replace = rewrite(&Command::ReplaceInput {
        target: target.clone(),
        text: String::new(),
        extras: Default::default(),
        restore_code: false,
        stop_cut_works: false,
    });
    let rewind_files = rewrite(&Command::Rewind {
        target: target.clone(),
        scope: RewindScope::Code,
    });
    // fork（§54.5）：回合在跑时也放开（不打断它，内核只分已经结束的一轮），有压缩时 `compacting`
    let fork = rewrite(&Command::ForkFromInputAs {
        input: target,
        session: SessionId::new(String::new()),
    });
    let mut gates = json!({
        "input": input,
        "stop": stop,
        "queueSendNow": allow(),
        "compact": compact,
        "edit": replace, "retry": replace, "rewindFiles": rewind_files, "fork": fork,
        "goalPause": unsupported, "goalResume": unsupported,
    });
    // 侧聊会话（§57.4）：这几条命令通道按种类拒绝，门与它一致
    if side_chat {
        for gate in super::side_chat::GATES {
            gates[gate] = deny(codes::ADMISSION_SIDE_CHAT);
        }
    }
    gates
}

/// `rowsWindow`（02 §1、§2.3）。
/// `rowsWindow`（02 §2.3，§73.3）：已知的行数、已知最早一行，与它之前还有没读的历史。
pub(crate) fn rows_window(total: usize, first: Option<&str>, earlier: bool) -> Value {
    json!({"totalCount": total, "firstId": first, "earlier": earlier})
}

/// `notices` 里一条的参数名（protocol-v5.md §33.1）：界面按种类取整句的文案、把它们插进去。
mod notice_params {
    /// Stop hook 连续阻塞的次数。
    pub(super) const COUNT: &str = "count";
    /// 调上限的环境变量名。
    pub(super) const VARIABLE: &str = "variable";
}

/// `notices` 里的一条（02 §11.9 第 3 项，protocol-v5.md §28.1、§33.1）：`id` 是这一帧的 topic 序号，只给界面认出新的一条；有种类时带上种类与参数，
/// 要按语言显示整句的界面据它取文案，`text` 仍是内核的原文。
pub(crate) fn notice(id: u64, level: NoticeLevel, text: &str, kind: Option<&NoticeKind>) -> Value {
    let level = match level {
        NoticeLevel::Info => SessionStateNoticesItemLevel::Info,
        NoticeLevel::Warning => SessionStateNoticesItemLevel::Warning,
    };
    let (kind, params) = match kind {
        Some(NoticeKind::StopHookLimit { blocked }) => (
            Some(SessionStateNoticesItemKind::StopHookLimit),
            Some(BTreeMap::from([
                (notice_params::COUNT.to_owned(), blocked.to_string()),
                (
                    notice_params::VARIABLE.to_owned(),
                    rust_cli_base::names::env::ZCODE_STOP_HOOK_BLOCK_CAP.to_owned(),
                ),
            ])),
        ),
        None => (None, None),
    };
    let item = SessionStateNoticesItem {
        id: id.to_string(),
        level,
        text: text.to_owned(),
        kind,
        params,
    };
    serde_json::to_value(item).unwrap_or(Value::Null)
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
