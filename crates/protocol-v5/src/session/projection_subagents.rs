//! 子代理行（protocol-v5.md §54；从 projection.rs 分出，800 行上限）：主线程的每个 Agent 调用派生的子代理一行 `subagent`，前台与后台都有。
//! 活会话的 `SubagentRecorded`、由视图建（视图的 `subagents`）与只在库里的（`Origin.subagents`，§45.3 的读法）都经 [`row`] 写行；行要配
//! Agent 的工具行（界面按 `(turnId, parentToolCallId)` 配），工具行不在（被改写历史截掉）就不出。

use rust_cli_kernel_api::agents::{StoredCall, StoredSubagent};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::subagent_runs::{SubagentOutcome, SubagentRun};

use super::*;
use crate::session::reads;

/// 行的种类与字段（契约 `RowSchema` 的 `subagent`）。
pub(super) const KIND: &str = "subagent";
mod fields {
    pub(super) const PARENT: &str = "parentToolCallId";
    pub(super) const SUMMARY: &str = "summaryText";
    pub(super) const CHILD: &str = "childSessionId";
    pub(super) const TYPE: &str = "subagentType";
    pub(super) const BACKGROUNDED: &str = "backgrounded";
    pub(super) const WORK: &str = "workId";
}
/// 行 id：`<派生它的调用 id>:subagent`（调用 id 落库不变；续跑仍是这一行）。
const ROW_SUFFIX: &str = ":subagent";

/// 一行的来源：内核的子代理表（本进程派生的），或库里的关联（冷会话、本进程之前的、Node 写的）。
#[derive(Debug, Clone)]
enum Source {
    Live(SubagentRun),
    Stored(StoredSubagent),
}

impl Source {
    fn tool_use(&self) -> &str {
        match self {
            Self::Live(run) => run.start.tool_use.as_str(),
            Self::Stored(stored) => stored.tool_use.as_str(),
        }
    }
}

/// 投影里的子代理：物化时读的库里的那份（改写历史之后重建时沿用），与由视图建时还没放的行。
#[derive(Debug, Default)]
pub(super) struct Subagents {
    stored: Vec<StoredSubagent>,
    /// 调用 id → 来源（视图的优先）：派生它的那条 assistant 消息的行建完之后放，放了就去掉。活会话里是空的。
    pending: BTreeMap<String, Source>,
}

impl Subagents {
    /// 视图的与库里的按子会话合并，视图的优先（同一个 actor 的寿命里重建不会把内核记下的结局换成库里推出来的，§54.1）。
    pub(super) fn new(view: &[SubagentRun], stored: Vec<StoredSubagent>) -> Self {
        let mut pending: BTreeMap<String, Source> = stored
            .iter()
            .filter(|stored| !view.iter().any(|run| run.start.child == stored.child))
            .map(|stored| {
                (
                    stored.tool_use.as_str().to_owned(),
                    Source::Stored(stored.clone()),
                )
            })
            .collect();
        for run in view {
            pending.insert(
                run.start.tool_use.as_str().to_owned(),
                Source::Live(run.clone()),
            );
        }
        Self { stored, pending }
    }

    pub(super) fn stored(&self) -> Vec<StoredSubagent> {
        self.stored.clone()
    }
}

fn outcome_status(outcome: SubagentOutcome) -> &'static str {
    match outcome {
        SubagentOutcome::Completed => turn_state::COMPLETED,
        SubagentOutcome::Failed => turn_state::ERROR,
        SubagentOutcome::Cancelled => turn_state::CANCELLED,
    }
}

/// 库里的调用的结局（§54.1）：失败里有停的词的是被停的（同 §45.3）；没有结果的调用读成取消（同没有结果的工具行，§31.1）。
fn stored_status(call: &StoredCall) -> &'static str {
    match call {
        StoredCall::Completed => turn_state::COMPLETED,
        StoredCall::Failed { error } if reads::cancelled(error) => turn_state::CANCELLED,
        StoredCall::Failed { .. } => turn_state::ERROR,
        StoredCall::Unfinished => turn_state::CANCELLED,
    }
}

/// 一行 `subagent`。
fn row(source: &Source, turn: &str, seq: u64) -> Fields {
    let (parent, child, agent_type, description) = match source {
        Source::Live(run) => (
            run.start.tool_use.as_str(),
            run.start.child.as_str(),
            run.start.agent_type.as_str(),
            run.start.description.as_str(),
        ),
        Source::Stored(stored) => (
            stored.tool_use.as_str(),
            stored.child.as_str(),
            stored.agent_type.as_deref().unwrap_or(reads::SUBAGENT_TYPE),
            stored.description.as_deref().unwrap_or_default(),
        ),
    };
    let (status, started_at, ended_at, work) = match source {
        Source::Live(run) => (
            run.ended
                .map_or(turn_state::RUNNING, |ended| outcome_status(ended.outcome)),
            Some(run.started_at),
            run.ended.map(|ended| ended.at),
            run.start.background.then_some(run.start.agent.as_str()),
        ),
        // 库里分不出前台后台：不写 `backgrounded`（只在库里的都已结束，界面只在 running 时看它）
        Source::Stored(stored) => (
            stored_status(&stored.call),
            stored.started_at_ms,
            stored.ended_at_ms,
            None,
        ),
    };
    let stable = status != turn_state::RUNNING;
    let mut fields = rows::base(KIND, turn, seq, stable, started_at);
    fields.insert(fields::PARENT.to_owned(), json!(parent));
    fields.insert(keys::STATUS.to_owned(), json!(status));
    // 照 v4：说明（运行中与结束之后都是它）
    fields.insert(fields::SUMMARY.to_owned(), json!(description));
    fields.insert(fields::CHILD.to_owned(), json!(child));
    fields.insert(fields::TYPE.to_owned(), json!(agent_type));
    if let Some(work) = work {
        fields.insert(fields::BACKGROUNDED.to_owned(), json!(true));
        fields.insert(fields::WORK.to_owned(), json!(work));
    }
    if let Some(at) = started_at {
        fields.insert(keys::STARTED_AT.to_owned(), rows::millis(at));
    }
    if let Some(at) = ended_at {
        fields.insert(keys::ENDED_AT.to_owned(), rows::millis(at));
    }
    fields
}

impl Projection {
    /// 活会话的 `SubagentRecorded`：同一个 id 整行替换（号不变）。
    pub(super) fn subagent_recorded(&mut self, edit: &mut Edit<'_>, run: &SubagentRun) {
        self.subagent_row(edit, &Source::Live(run.clone()));
    }

    /// 由视图建：一条 assistant 消息的行建完之后，它里面的 Agent 调用派生的子代理（§54.3）。活会话里没有待放的。
    pub(super) fn message_subagents(&mut self, edit: &mut Edit<'_>, message: &Message) {
        let Message::Assistant(assistant) = message else {
            return;
        };
        for block in &assistant.content {
            if let ContentBlock::ToolUse { id, .. } = block
                && let Some(source) = self.subagents.pending.remove(id.as_str())
            {
                self.subagent_row(edit, &source);
            }
        }
    }

    /// 放一行：Agent 的工具行不在时不放（行要配它）。
    fn subagent_row(&mut self, edit: &mut Edit<'_>, source: &Source) {
        let tool_use = source.tool_use();
        let Some(turn) = edit
            .get(ROWS, tool_use)
            .map(|tool| rows::text(tool, keys::TURN_ID).to_owned())
        else {
            return;
        };
        let id = format!("{tool_use}{ROW_SUFFIX}");
        let seq = self.seq_of(edit, &id);
        edit.put(ROWS, &id, row(source, &turn, seq));
    }
}

#[cfg(test)]
#[path = "projection_subagents_tests.rs"]
mod tests;
