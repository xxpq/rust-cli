//! hook 行（protocol-v5.md §49；从 projection.rs 分出，800 行上限）：一次 hook 事件一行 `hookInvocation`，照 v4 的形状（v4 投影的
//! `onHookRunLifecycle`，出处在 §49.1）；UserPromptSubmit 拦下时的横幅（`hookBlockErrorDelta`）；退出码 2 拦下的输入的用户行。活会话的
//! `HooksRan` 与由视图建（视图的 `hooks`）同一组函数。

use rust_cli_kernel_api::hook_runs::{HookOrigin, HookRun, HookRunOutcome, TurnHooks};
use rust_cli_kernel_api::hooks::{HookEventName, NODE_EVENTS};

use super::*;

/// 行的种类与字段（契约 `RowSchema` 的 `hookInvocation`）。
pub(super) const KIND: &str = "hookInvocation";
mod fields {
    pub(super) const EVENT: &str = "hookEventName";
    pub(super) const LANE: &str = "lane";
    pub(super) const COUNT: &str = "hookCount";
    pub(super) const DURATION: &str = "durationMs";
    pub(super) const ANCHOR: &str = "anchorToolCallId";
    pub(super) const EXECUTIONS: &str = "executions";
}

/// 一项 `executions`（v4 的 `hookExecutionProjectionSchema`，strict）的键。
mod execution {
    pub(super) const RUN_ID: &str = "hookRunId";
    pub(super) const INDEX: &str = "hookIndex";
    pub(super) const DID_EXECUTE: &str = "didExecute";
    pub(super) const STATE: &str = "state";
    pub(super) const OUTCOME: &str = "outcome";
    pub(super) const BLOCK_REASON: &str = "blockReason";
    pub(super) const STARTED_AT: &str = "startedAt";
    pub(super) const ENDED_AT: &str = "endedAt";
    pub(super) const DURATION: &str = "durationMs";
    pub(super) const DISPLAY_NAME: &str = "displayName";
    pub(super) const SOURCE_KIND: &str = "sourceKind";
    pub(super) const PLUGIN_NAME: &str = "pluginName";
    pub(super) const TOOL_NAME: &str = "toolName";
}

/// 行与每一项的状态（v4 的 `HOOK_INVOCATION_STATES`；没有 `running`：事实到的时候 hook 都结束了，§49.7 第 1 项）。
const COMPLETED: &str = "completed";
const FAILED: &str = "failed";

/// 泳道（v4 的 `hookInvocationLane`）。
const LANE_TOOL_BEFORE: &str = "toolBefore";
const LANE_TOOL_AFTER: &str = "toolAfter";
const LANE_ASSISTANT_WORK: &str = "assistantWork";

/// 来源（v4 的 `sourceKind`）。
const SOURCE_USER: &str = "user";
const SOURCE_PROJECT: &str = "project";
const SOURCE_PLUGIN: &str = "plugin";

/// 显示名里的分隔与没有名字时的写法（v4 的 `hookExecutionDisplayName`）。
const NAME_SEPARATOR: &str = " · ";
const UNNAMED_PREFIX: &str = "Hook #";
/// 脚本解释器（v4 的 `HOOK_SCRIPT_RUNNERS`）：可执行文件是它们时再接脚本的文件名。
const SCRIPT_RUNNERS: &[&str] = &[
    "bash",
    "bun",
    "deno",
    "node",
    "node.exe",
    "powershell",
    "pwsh",
    "python",
    "python3",
    "ruby",
    "sh",
    "zsh",
];

/// 横幅（v4 的 `hookBlockErrorDelta`）：`message` 的前缀与 `detail` 的两行。
const PROMPT_BLOCK: &str = "hooks_prompt_block";
const DETAIL_REASON: &str = "Hook block reason: ";
const DETAIL_ERROR: &str = "Hook error: ";

/// 这一轮的 UserPromptSubmit 拦下（`lastError`）：回合结束时 [`Projection::finished`] 问它，成功结束时横幅留着。
#[derive(Debug)]
pub(super) struct HookBlock {
    pub(super) turn: String,
    pub(super) error: Value,
}

fn row_id(hooks: &TurnHooks) -> String {
    format!("{}:hooks:{}", hooks.turn.as_str(), hooks.seq)
}

fn lane(event: HookEventName) -> &'static str {
    match event {
        HookEventName::PreToolUse | HookEventName::PermissionRequest => LANE_TOOL_BEFORE,
        HookEventName::PostToolUse | HookEventName::PostToolUseFailure => LANE_TOOL_AFTER,
        _ => LANE_ASSISTANT_WORK,
    }
}

fn failed(run: &HookRun) -> bool {
    matches!(
        run.outcome,
        HookRunOutcome::Failed | HookRunOutcome::Cancelled | HookRunOutcome::TimedOut
    )
}

fn outcome(run: &HookRun) -> &'static str {
    match run.outcome {
        HookRunOutcome::Success => "success",
        HookRunOutcome::Blocked => "blocked",
        HookRunOutcome::Failed => "failed",
        HookRunOutcome::Cancelled => "cancelled",
        HookRunOutcome::TimedOut => "timed_out",
    }
}

/// v4 的 `unquoteHookDisplayToken`：双引号的按 JSON 读（读不出时去掉引号），单引号的去掉引号。
fn unquote(token: &str) -> String {
    let inner = |quote: char| {
        token
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
    };
    if let Some(inner) = inner('"') {
        return serde_json::from_str(token).unwrap_or_else(|_| inner.to_owned());
    }
    inner('\'').unwrap_or(token).to_owned()
}

/// v4 的 `/"(?:\\.|[^"])*"|'[^']*'|\S+/gu`：带引号的整段，否则到下一个空白。没有收尾的引号时按不带引号的一段。
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text.trim_start();
    while let Some(first) = rest.chars().next() {
        let quoted = match first {
            '"' => {
                let mut escaped = false;
                rest.char_indices().skip(1).find_map(|(at, c)| {
                    let end = !escaped && c == '"';
                    escaped = !escaped && c == '\\';
                    end.then_some(at + 1)
                })
            }
            '\'' => {
                let quote = first.len_utf8();
                rest[quote..].find('\'').map(|at| quote + at + quote)
            }
            _ => None,
        };
        let end = quoted.unwrap_or_else(|| rest.find(char::is_whitespace).unwrap_or(rest.len()));
        out.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    out
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// v4 的 `hookCommandLabel`：可执行文件的名字；是脚本解释器时接上脚本的文件名。
fn command_label(command: &str) -> Option<String> {
    let tokens = tokens(command);
    let executable = unquote(tokens.first()?);
    let executable = file_name(&executable);
    if executable.is_empty() {
        return None;
    }
    let script = tokens.get(1).map(|token| unquote(token));
    match script {
        Some(script)
            if SCRIPT_RUNNERS.contains(&executable.to_lowercase().as_str())
                && !script.is_empty()
                && !script.starts_with('-') =>
        {
            Some(format!(
                "{executable}{NAME_SEPARATOR}{}",
                file_name(&script)
            ))
        }
        _ => Some(executable.to_owned()),
    }
}

/// v4 的 `hookExecutionDisplayName`：`statusMessage` → `<插件名> · <可执行文件>` → 插件名 → 可执行文件 → `Hook #<序号 + 1>`。
pub(super) fn display_name(run: &HookRun) -> String {
    let status = run
        .status_message
        .as_deref()
        .map(str::trim)
        .filter(|status| !status.is_empty());
    let plugin = match &run.origin {
        HookOrigin::Plugin { name } => Some(name.as_str()),
        _ => None,
    };
    let executable = command_label(&run.command);
    status
        .map(str::to_owned)
        .or_else(|| match (plugin, &executable) {
            (Some(plugin), Some(executable)) => {
                Some(format!("{plugin}{NAME_SEPARATOR}{executable}"))
            }
            (Some(plugin), None) => Some(plugin.to_owned()),
            (None, executable) => executable.clone(),
        })
        .unwrap_or_else(|| format!("{UNNAMED_PREFIX}{}", run.index + 1))
}

fn source_kind(origin: &HookOrigin) -> &'static str {
    match origin {
        HookOrigin::User | HookOrigin::Policy => SOURCE_USER,
        HookOrigin::Project | HookOrigin::Local => SOURCE_PROJECT,
        HookOrigin::Plugin { .. } => SOURCE_PLUGIN,
    }
}

/// 一项 `executions`。
fn execution(row: &str, run: &HookRun, tool: Option<&str>) -> Value {
    let mut item = Fields::new();
    item.insert(
        execution::RUN_ID.to_owned(),
        json!(format!("{row}:{}", run.index)),
    );
    item.insert(execution::INDEX.to_owned(), json!(run.index));
    // 未信任的项目 hook 不进快照，没有“没执行”的项
    item.insert(execution::DID_EXECUTE.to_owned(), json!(true));
    let state = if failed(run) { FAILED } else { COMPLETED };
    item.insert(execution::STATE.to_owned(), json!(state));
    item.insert(execution::OUTCOME.to_owned(), json!(outcome(run)));
    if run.outcome == HookRunOutcome::Blocked
        && let Some(reason) = run
            .reason
            .as_ref()
            .filter(|reason| !reason.trim().is_empty())
    {
        item.insert(execution::BLOCK_REASON.to_owned(), json!(reason));
    }
    item.insert(
        execution::STARTED_AT.to_owned(),
        rows::millis(run.started_at),
    );
    item.insert(execution::ENDED_AT.to_owned(), rows::millis(run.ended_at));
    let duration = run.ended_at.saturating_sub(run.started_at);
    item.insert(execution::DURATION.to_owned(), rows::millis(duration));
    item.insert(execution::DISPLAY_NAME.to_owned(), json!(display_name(run)));
    item.insert(
        execution::SOURCE_KIND.to_owned(),
        json!(source_kind(&run.origin)),
    );
    if let HookOrigin::Plugin { name } = &run.origin {
        item.insert(execution::PLUGIN_NAME.to_owned(), json!(name));
    }
    if let Some(tool) = tool {
        item.insert(execution::TOOL_NAME.to_owned(), json!(tool));
    }
    json!(item)
}

/// 这次事件的开始与结束：最早的开始、最晚的结束；没有跑 hook 时没有（不成行）。
fn span(runs: &[HookRun]) -> Option<(u64, u64)> {
    let started = runs.iter().map(|run| run.started_at).min()?;
    let ended = runs.iter().map(|run| run.ended_at).max()?;
    Some((started, ended))
}

/// 一行 `hookInvocation`。
fn hook_row(hooks: &TurnHooks, seq: u64, (started, ended): (u64, u64)) -> Fields {
    let runs = &hooks.invocation.runs;
    let id = row_id(hooks);
    let event = hooks.invocation.event;
    let mut fields = rows::base(KIND, hooks.turn.as_str(), seq, true, Some(started));
    fields.insert(fields::EVENT.to_owned(), json!(event.as_str()));
    fields.insert(fields::LANE.to_owned(), json!(lane(event)));
    fields.insert(fields::COUNT.to_owned(), json!(runs.len()));
    let state = if runs.iter().any(failed) {
        FAILED
    } else {
        COMPLETED
    };
    fields.insert(keys::STATE.to_owned(), json!(state));
    fields.insert(keys::STARTED_AT.to_owned(), rows::millis(started));
    fields.insert(keys::ENDED_AT.to_owned(), rows::millis(ended));
    fields.insert(fields::DURATION.to_owned(), rows::millis(ended - started));
    let tool = hooks.invocation.tool.as_ref();
    if let Some(tool) = tool {
        fields.insert(fields::ANCHOR.to_owned(), json!(tool.tool_use.as_str()));
    }
    let tool = tool.map(|tool| tool.name.as_str());
    let executions: Vec<Value> = runs.iter().map(|run| execution(&id, run, tool)).collect();
    fields.insert(fields::EXECUTIONS.to_owned(), Value::Array(executions));
    fields
}

/// 横幅的 `message` 与 `detail`（v4 的 `hookBlockErrorDelta`）：诊断取 stderr（与原因不同时），否则是原因。
fn banner(run: &HookRun) -> (String, String) {
    let reason = run.reason.clone().unwrap_or_default();
    let diagnostic = run
        .stderr
        .as_deref()
        .map(str::trim)
        .filter(|stderr| !stderr.is_empty() && *stderr != reason);
    let shown = diagnostic.unwrap_or(&reason);
    let message = if shown == PROMPT_BLOCK {
        PROMPT_BLOCK.to_owned()
    } else {
        format!("{PROMPT_BLOCK}: {shown}")
    };
    let mut detail = format!("{DETAIL_REASON}{reason}");
    if let Some(diagnostic) = diagnostic {
        detail.push('\n');
        detail.push_str(DETAIL_ERROR);
        detail.push_str(diagnostic);
    }
    (message, detail)
}

impl Projection {
    /// 活会话的 `HooksRan`：行；UserPromptSubmit 拦下时的横幅；被拦下的输入的用户行（同一批）。
    pub(super) fn hooks_ran(&mut self, edit: &mut Edit<'_>, hooks: &TurnHooks) {
        if !self.hook_row(edit, hooks) {
            return;
        }
        let turn = hooks.turn.as_str();
        if let Some(blocked) = &hooks.blocked {
            let id = blocked.message.as_str();
            let seq = self.seq_of(edit, id);
            let mut fields = rows::user_input(
                &blocked.text,
                false,
                turn,
                seq,
                blocked.message.created_ms(),
            );
            // 这条输入的命令：回合头带着它（`TurnStarted.command`）
            if let Some(command) = edit
                .get(ROWS, turn)
                .and_then(|header| header.get(keys::SOURCE_COMMAND_ID).cloned())
            {
                fields.insert(keys::SOURCE_COMMAND_ID.to_owned(), command);
            }
            edit.put(ROWS, id, fields);
        }
        let blocking = hooks
            .invocation
            .runs
            .iter()
            .filter(|run| run.outcome == HookRunOutcome::Blocked)
            .min_by_key(|run| run.index);
        if hooks.invocation.event == HookEventName::UserPromptSubmit
            && let Some(run) = blocking
        {
            let (message, detail) = banner(run);
            let attribution = self.attribution(edit, turn);
            let error = state::hook_blocked(&message, &detail, run.ended_at, attribution);
            self.error = Some(error.clone());
            self.hook_block = Some(HookBlock {
                turn: turn.to_owned(),
                error,
            });
            self.control(edit);
        }
    }

    /// 由视图建：这个回合的 hook 行（回合头建好之后调；没有回合头的回合不放）。
    pub(super) fn turn_hooks(&mut self, edit: &mut Edit<'_>, hooks: &[TurnHooks], turn: &str) {
        for hooks in hooks.iter().filter(|hooks| hooks.turn.as_str() == turn) {
            self.hook_row(edit, hooks);
        }
    }

    /// 放一行；v4 有行的 7 个事件以外、没有跑 hook 的不画（返回 false）。
    fn hook_row(&mut self, edit: &mut Edit<'_>, hooks: &TurnHooks) -> bool {
        let Some(span) = span(&hooks.invocation.runs) else {
            return false;
        };
        if !NODE_EVENTS.contains(&hooks.invocation.event) {
            return false;
        }
        let id = row_id(hooks);
        let seq = self.seq_of(edit, &id);
        edit.put(ROWS, &id, hook_row(hooks, seq, span));
        true
    }
}
