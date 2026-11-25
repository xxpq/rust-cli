//! 会话的行（02 §5；protocol-v5.md §15.3.3）：一条消息给出哪几行、每行的 id 与字段。冷读（`SessionView.messages`）与活会话
//! （`MessageDone`）走同一个函数，所以同一条消息在两边是同一组行。
//!
//! 行 id 由持久的 id 派生（K-I2）：用户输入 = message id；回答正文 = assistant 的 message id；思考 = `<message id>:reasoning:<k>`；
//! 工具调用 = `tool_use` id；回合头 = 回合 id；压缩标记 = 边界消息的 id。Node 的落库形状给了稳定的槽位：一条 assistant 一个
//! text part（全部文字块拼接）、每个推理块一个 reasoning part、每个 tool_use 一个 tool part（node-store `append_assistant`）。

use rust_cli_base::line_diff::Hunk;
use rust_cli_kernel_api::background::NoticedWork;
use rust_cli_kernel_api::command_output::CommandOutputHead;
use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::message::{
    AttachmentMessage, CompactTrigger, ContentBlock, Message, SystemMessage, UserOrigin, text_of,
};
use rust_cli_kernel_api::plan::ShownPlan;
use rust_cli_kernel_api::session_api::{InputEnd, TurnFileChange, TurnOrigin, TurnTime};
use serde_json::{Value, json};

use crate::generated::session_v1::{
    CompactMarkerDetail, CompactMarkerDetailOrigin, CompactMarkerDetailStatus,
    CompactMarkerDetailType, QuerySessionFileChangesResult, QuerySessionFileChangesResultItemsItem,
    QuerySessionFileChangesResultItemsItemPatchesItem, RowTurnHeaderFileChangeSummary,
    RowTurnHeaderFileChangeSummaryState, WorkKind,
};
use crate::publisher::Fields;

/// 行集合（02 §1）。
pub(crate) const ROWS: &str = "rows";
/// 行的插入序号字段（02 §1.3）：快照窗口按它排序。
pub(crate) const CREATED_AT_SEQ: &str = "createdAtSeq";
/// 思考行 id 的中段：`<message id>:reasoning:<k>`。
const REASONING_SLOT: &str = "reasoning";

/// 行的种类（`RowSchema` 的 `kind`）。
pub(crate) mod kinds {
    pub(crate) const TURN_HEADER: &str = "turnHeader";
    pub(crate) const USER_INPUT: &str = "userInput";
    pub(crate) const ASSISTANT_TEXT: &str = "assistantText";
    pub(crate) const REASONING: &str = "reasoning";
    pub(crate) const TOOL_CALL: &str = "toolCall";
    pub(crate) const TIMELINE_MARKER: &str = "timelineMarker";
}

/// 流式行与回答的状态（`assistantText.state` / `reasoning.state`）。
pub(crate) const STREAMING: &str = "streaming";
pub(crate) const DONE: &str = "done";

/// 工具行的状态（`TOOL_CALL_STATUSES`）。
pub(crate) mod tool_status {
    pub(crate) const INPUT_STREAMING: &str = "inputStreaming";
    pub(crate) const PENDING: &str = "pending";
    pub(crate) const PENDING_APPROVAL: &str = "pendingApproval";
    pub(crate) const RUNNING: &str = "running";
    pub(crate) const COMPLETED: &str = "completed";
    pub(crate) const ERROR: &str = "error";
    pub(crate) const CANCELLED: &str = "cancelled";

    /// 已有结局的状态：之后的消息与事件不再改它。
    pub(crate) fn ended(status: &str) -> bool {
        matches!(status, COMPLETED | ERROR | CANCELLED)
    }
}

/// 回合头的状态（`TURN_STATES`）。
pub(crate) mod turn_state {
    pub(crate) const RUNNING: &str = "running";
    pub(crate) const COMPLETED: &str = "completed";
    pub(crate) const ERROR: &str = "error";
    pub(crate) const CANCELLED: &str = "cancelled";
}

/// 字段名（`RowSchema` 的键）。
pub(crate) mod keys {
    pub(crate) const KIND: &str = "kind";
    pub(crate) const TURN_ID: &str = "turnId";
    pub(crate) const STABLE: &str = "stable";
    pub(crate) const CREATED_AT: &str = "createdAt";
    pub(crate) const TEXT: &str = "text";
    pub(crate) const STATE: &str = "state";
    pub(crate) const STATUS: &str = "status";
    pub(crate) const TOOL_NAME: &str = "toolName";
    pub(crate) const INPUT_TEXT: &str = "inputText";
    pub(crate) const INPUT: &str = "input";
    pub(crate) const OUTPUT: &str = "output";
    pub(crate) const ERROR: &str = "error";
    pub(crate) const APPROVAL: &str = "approvalInteractionId";
    pub(crate) const OUTPUT_PREVIEW: &str = "outputPreview";
    pub(crate) const RESPONSE: &str = "assistantResponseId";
    pub(crate) const MODEL: &str = "model";
    pub(crate) const ORIGIN: &str = "origin";
    pub(crate) const GUIDED: &str = "guided";
    pub(crate) const STARTED_AT: &str = "startedAt";
    /// 回合头的结束时刻与耗时（protocol-v5.md §29.1）。
    pub(crate) const ENDED_AT: &str = "endedAt";
    pub(crate) const DURATIONS: &str = "durations";
    pub(crate) const MARKER: &str = "marker";
    pub(crate) const DETAIL: &str = "detail";
    /// 工具结果 `output` 里只给界面的数据（v4 `toolOutputSchema.display`）。
    pub(crate) const DISPLAY: &str = "display";
    /// 产生这一行的命令（protocol-v5.md §24）。
    pub(crate) const SOURCE_COMMAND_ID: &str = "sourceCommandId";
    /// 回合头的文件改动（protocol-v5.md §42.1）：汇总与每个文件一项。
    pub(crate) const FILE_CHANGE_SUMMARY: &str = "fileChangeSummary";
    pub(crate) const FILE_CHANGES: &str = "fileChanges";
    /// 输入行的高速卡（protocol-v5.md §46.1）。
    pub(crate) const HIGHSPEED: &str = "highspeed";
    /// 后台结果开的回合的来源（protocol-v5.md §47.2）。
    pub(crate) const ORIGIN_META: &str = "originMeta";
}

/// 回合头的 `originMeta`（v4 的 `backgroundResultOriginMetaSchema`，protocol-v5.md §47.2）：键与几项合成一个标题的写法。
mod origin_meta {
    pub(super) const BACKGROUND_SOURCE: &str = "backgroundSource";
    pub(super) const WORK_ID: &str = "workId";
    pub(super) const TITLE: &str = "title";
    /// 一轮合了几条通知时标题最多列几项（同 v4 的 `MAX_BACKGROUND_RESULT_TITLES`）。
    pub(super) const MAX_TITLES: usize = 3;
    pub(super) const TITLE_SEPARATOR: &str = " · ";
    /// 多出的项数的写法：`+N`。
    pub(super) const MORE_PREFIX: &str = "+";
}

/// 输入行 `highspeed` 里的退回事实（契约 `HighspeedRow`，同 v4 `highspeedMessageMetaSchema`）。
mod fallback {
    pub(super) const AT: &str = "fallbackAt";
    pub(super) const REASON: &str = "fallbackReason";
}

/// ExitPlanMode 行的 `input` 里的计划（protocol-v5.md §42.1）：工具读到的计划与计划文件，界面的计划目录与计划卡读这两个键。
mod plan_input {
    pub(super) const PLAN: &str = "plan";
    pub(super) const PLAN_FILE_PATH: &str = "planFilePath";
}

/// Bash 输出头部的 display（protocol-v5.md §21）：v4 的 `bashOutputDisplaySchema`（`shared/src/bash-output-display.ts`，strict）。
mod bash_display {
    pub(super) const KIND: &str = "kind";
    pub(super) const BASH_OUTPUT: &str = "bash_output";
    pub(super) const OUTPUT: &str = "output";
    pub(super) const TRUNCATED: &str = "truncated";
    pub(super) const OUTPUT_PATH: &str = "outputPath";
}

/// 输出头部换成 v4 的 `bash_output` display。`truncated` 恒为真：内核只在输出超过内联上限时给头部，其余在输出文件里。
pub(crate) fn bash_display(head: CommandOutputHead) -> Value {
    json!({
        bash_display::KIND: bash_display::BASH_OUTPUT,
        bash_display::OUTPUT: head.head,
        bash_display::TRUNCATED: true,
        bash_display::OUTPUT_PATH: head.output_file,
    })
}

/// 压缩标记（`TIMELINE_MARKERS`）。
const COMPACT_MARKER: &str = "compact";
/// 压缩的回合在跑与失败时的标记的行 id：`<回合 id>` 加这个后缀（protocol-v5.md §23.1、§47.1）。这两态没有边界消息，回合 id 是持久的事实，
/// 一个压缩的回合只压缩一次。
const TURN_COMPACTION_SUFFIX: &str = ":compact";
/// 用户输入行的来源（`TURN_ORIGINS`）。
const USER_ORIGIN: &str = "user";

/// 一条消息给出的一项：显示的行，或对已有工具行的结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Part {
    User {
        id: String,
        text: String,
        guided: bool,
    },
    Reasoning {
        id: String,
        text: String,
    },
    Text {
        id: String,
        text: String,
        model: String,
    },
    Tool {
        id: String,
        name: String,
        input: Value,
    },
    /// 工具结果（user 消息里的 tool_result）：更新同 id 的工具行。
    Result {
        id: String,
        text: String,
        is_error: bool,
    },
    /// 压缩边界：怎么触发的、压缩前的 token 数（`CompactBoundary`）。
    Marker {
        id: String,
        trigger: CompactTrigger,
        tokens_before: u64,
    },
}

/// 思考行的 id。
pub(crate) fn reasoning_id(message: &MessageId, k: usize) -> String {
    format!("{message}:{REASONING_SLOT}:{k}")
}

/// 显示的思考块：有文字或签名的 thinking，与 redacted thinking（同 node-store 落库时的筛选：没有文字也没有签名的不写）。
fn displayed_thinking(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Thinking {
            thinking,
            signature,
        } if !thinking.is_empty() || signature.is_some() => Some(thinking),
        ContentBlock::RedactedThinking { .. } => Some(""),
        _ => None,
    }
}

/// 用户输入的显示文字：文字块按换行接起，命令标签与 bash 模式还原（同 node-store 落库的 `display_input`）。
fn user_text(content: &[ContentBlock]) -> String {
    let text = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    rust_cli_kernel_api::user_command::display(&text)
        .or_else(|| rust_cli_kernel_api::user_input::display(&text))
        .unwrap_or(text)
}

/// 工具边界并入的插话（`queued_command` attachment 的 prompt 模式）：是一行用户输入，`guided: true`。
fn guided(attachment: &AttachmentMessage) -> Option<Part> {
    if attachment.source != queued_command::SOURCE
        || attachment
            .data
            .get(queued_command::MODE_KEY)
            .and_then(Value::as_str)
            != Some(queued_command::PROMPT_MODE)
    {
        return None;
    }
    let text = attachment
        .data
        .get(queued_command::PROMPT_KEY)
        .and_then(Value::as_str)?;
    Some(Part::User {
        id: attachment.id.as_str().to_owned(),
        text: text.to_owned(),
        guided: true,
    })
}

/// 一条消息给出的各项，按显示顺序：思考、正文、工具调用（同冷读的顺序：tool_use 在后）。
pub(crate) fn parts(message: &Message) -> Vec<Part> {
    match message {
        Message::User(user) => match &user.origin {
            // 中断后补的提示与 bash 模式的输出也是 Human 来源，但不是用户输入（同存储划分轮次的判定）
            UserOrigin::Human if !rust_cli_kernel_api::user_input::opens_turn(user) => Vec::new(),
            UserOrigin::Human => vec![Part::User {
                id: user.id.as_str().to_owned(),
                text: user_text(&user.content),
                guided: false,
            }],
            UserOrigin::ToolResult { .. } => user
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => Some(Part::Result {
                        id: tool_use_id.as_str().to_owned(),
                        text: text_of(content),
                        is_error: *is_error,
                    }),
                    _ => None,
                })
                .collect(),
            UserOrigin::Meta { .. } | UserOrigin::CompactSummary => Vec::new(),
        },
        Message::Assistant(assistant) if assistant.is_resume_sentinel() => Vec::new(),
        Message::Assistant(assistant) => {
            let mut parts: Vec<Part> = assistant
                .content
                .iter()
                .filter_map(displayed_thinking)
                .enumerate()
                .map(|(k, text)| Part::Reasoning {
                    id: reasoning_id(&assistant.id, k),
                    text: text.to_owned(),
                })
                .collect();
            let text: String = assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                parts.push(Part::Text {
                    id: assistant.id.as_str().to_owned(),
                    text,
                    model: assistant.model.clone(),
                });
            }
            parts.extend(assistant.content.iter().filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(Part::Tool {
                    id: id.as_str().to_owned(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            }));
            parts
        }
        Message::Attachment(attachment) => guided(attachment).into_iter().collect(),
        Message::System(SystemMessage::CompactBoundary(boundary)) => vec![Part::Marker {
            id: boundary.id.as_str().to_owned(),
            trigger: boundary.trigger,
            tokens_before: boundary.pre_tokens,
        }],
        Message::System(SystemMessage::Interrupted { .. }) => Vec::new(),
    }
}

/// 计数 → 契约里的数字字段（f64）：行数与文件数远小于 2^53，精确。
#[expect(
    clippy::cast_precision_loss,
    reason = "行数与文件数远小于 2^53，f64 能精确表示"
)]
fn count(n: u64) -> f64 {
    n as f64
}

/// 一个文件的明细（`session.fileChanges` 的 items 与回合头的 `fileChanges` 同一个类型）：数字取回合记录的那一项。
pub(crate) fn file_item(
    file: &TurnFileChange,
    patches: Vec<QuerySessionFileChangesResultItemsItemPatchesItem>,
) -> QuerySessionFileChangesResultItemsItem {
    QuerySessionFileChangesResultItemsItem {
        path: file.change.path.clone(),
        additions: file.change.additions,
        deletions: file.change.deletions,
        write_count: file.writes,
        tool_names: file.tools.clone(),
        patches,
    }
}

/// 一个 hunk 换成契约的 `DiffHunkSchema`（jsdiff 的形状，§44.2）。
pub(crate) fn file_patch(hunk: &Hunk) -> QuerySessionFileChangesResultItemsItemPatchesItem {
    QuerySessionFileChangesResultItemsItemPatchesItem {
        old_start: count(hunk.old_start as u64),
        old_lines: count(hunk.old_lines as u64),
        new_start: count(hunk.new_start as u64),
        new_lines: count(hunk.new_lines as u64),
        lines: hunk.lines.clone(),
    }
}

/// 回合头的文件改动（protocol-v5.md §42.1）：活会话的 `TurnFinished.files` 与冷读的回合记录同一个函数。明细与查询 `session.fileChanges` 同一个类型，
/// 回合头不带 `patches`（行要小，查询时才给，§44），汇总由明细算。没有改动时不写（同 v4：文件数为 0 时回合头没有它）。
pub(crate) fn set_files(fields: &mut Fields, files: &[TurnFileChange]) {
    if files.is_empty() {
        return;
    }
    let items: Vec<QuerySessionFileChangesResultItemsItem> = files
        .iter()
        .map(|file| file_item(file, Vec::new()))
        .collect();
    let summary = file_changes(&items);
    fields.insert(
        keys::FILE_CHANGE_SUMMARY.to_owned(),
        json!(RowTurnHeaderFileChangeSummary {
            additions: count(summary.additions),
            deletions: count(summary.deletions),
            files: count(summary.files),
            state: RowTurnHeaderFileChangeSummaryState::Active,
        }),
    );
    fields.insert(keys::FILE_CHANGES.to_owned(), json!(items));
}

/// 回合头的文件改动标成已撤销（§53.5）：有汇总时改它的状态、返回真；没有文件改动的回合头不动。
pub(crate) fn set_reverted(fields: &mut Fields) -> bool {
    let Some(summary) = fields.get_mut(keys::FILE_CHANGE_SUMMARY) else {
        return false;
    };
    summary[file_change_summary::STATE] = json!(RowTurnHeaderFileChangeSummaryState::Reverted);
    true
}

/// 回合头的文件改动已撤销（`session.fileChanges` 的答案带同一个状态，§53.5）。
pub(crate) fn reverted(fields: &Fields) -> bool {
    fields
        .get(keys::FILE_CHANGE_SUMMARY)
        .and_then(|summary| summary.get(file_change_summary::STATE))
        == Some(&json!(RowTurnHeaderFileChangeSummaryState::Reverted))
}

/// 回合头 `fileChangeSummary` 的键。
mod file_change_summary {
    pub(super) const STATE: &str = "state";
}

/// 一组明细的汇总（`session.fileChanges` 的答案）；没有明细时是空的汇总、不带状态。
pub(crate) fn file_changes(
    items: &[QuerySessionFileChangesResultItemsItem],
) -> QuerySessionFileChangesResult {
    QuerySessionFileChangesResult {
        files: items.len() as u64,
        additions: items.iter().map(|item| item.additions).sum(),
        deletions: items.iter().map(|item| item.deletions).sum(),
        state: (!items.is_empty()).then_some(RowTurnHeaderFileChangeSummaryState::Active),
        items: items.to_vec(),
    }
}

/// ExitPlanMode 行的 `input` 带上工具拿给用户批准的计划（§42.1）：`inputText` 不动（模型的原始输入）。
pub(crate) fn set_plan(fields: &mut Fields, plan: &ShownPlan) {
    if let Some(input) = fields
        .entry(keys::INPUT.to_owned())
        .or_insert_with(|| json!({}))
        .as_object_mut()
    {
        input.insert(plan_input::PLAN.to_owned(), json!(plan.plan));
        input.insert(plan_input::PLAN_FILE_PATH.to_owned(), json!(plan.path));
    }
}

/// 计划目录里的一行要有计划文字（v4 `hasPlanMarkdown` 的条件，§42.1）：`input.plan` 去掉空白后不为空。
pub(crate) fn has_plan(row: &Value) -> bool {
    row.get(keys::INPUT)
        .and_then(|input| input.get(plan_input::PLAN))
        .and_then(Value::as_str)
        .is_some_and(|plan| !plan.trim().is_empty())
}

/// 毫秒时刻 → 线上的数字（毫秒数远小于 2^53，f64 精确）。
#[expect(
    clippy::cast_precision_loss,
    reason = "毫秒时间远小于 2^53，f64 能精确表示"
)]
pub(crate) fn millis(ms: u64) -> Value {
    json!(ms as f64)
}

/// 每行都有的字段（`rowBase`）。
/// 回合的开头（protocol-v5.md §71.1）：回合头，或不属于任何回合的行（`turnId` 为空）。快照的尾窗与 `rows.range` 的一页从取出的行里的第一个开头起，
/// 界面的「窗口首行所在的回合没有回合头就再补一页」因此不会一页接一页地成立。读快照里的写法（骨架也有这两个键）。
pub(crate) fn opens(item: &Value) -> bool {
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
    text(keys::KIND) == kinds::TURN_HEADER || text(keys::TURN_ID).is_empty()
}

pub(crate) fn base(
    kind: &str,
    turn: &str,
    seq: u64,
    stable: bool,
    created_at: Option<u64>,
) -> Fields {
    let mut fields = Fields::new();
    fields.insert(keys::KIND.to_owned(), json!(kind));
    fields.insert(keys::TURN_ID.to_owned(), json!(turn));
    fields.insert(CREATED_AT_SEQ.to_owned(), json!(seq));
    fields.insert(keys::STABLE.to_owned(), json!(stable));
    if let Some(at) = created_at {
        fields.insert(keys::CREATED_AT.to_owned(), millis(at));
    }
    fields
}

/// 后台工作的种类 → `works` 集合的 `kind` 与回合头 `originMeta.backgroundSource`（同一组值，§20.1、§47.2）：monitor 归 bash（照 v4：v4 没有这一种，
/// 它的后台任务按 bash 分组）。
pub(crate) fn work_kind(kind: BackgroundKind) -> WorkKind {
    match kind {
        BackgroundKind::Shell | BackgroundKind::Monitor => WorkKind::Bash,
        BackgroundKind::Agent => WorkKind::Subagent,
    }
}

/// 回合头带开这一轮的后台工作（§47.2，照 v4 合并几条通知的写法）：一项时是它的来源、ID 与说明；几项时来源与 ID 取第一项，标题是前三项的说明用
/// 「 · 」接起，多出的写「+N」。说明去掉空白后为空时用 ID（v4 同样退回任务 ID）。别的回合没有后台工作，不写。活会话（`TurnStarted.works`）与冷读
/// （回合记录的 `works`）都经回合头的这一个函数。
pub(crate) fn set_origin_meta(fields: &mut Fields, works: &[NoticedWork]) {
    let Some(first) = works.first() else {
        return;
    };
    let mut titles: Vec<String> = works
        .iter()
        .take(origin_meta::MAX_TITLES)
        .map(|work| match work.description.trim() {
            "" => work.id.clone(),
            title => title.to_owned(),
        })
        .collect();
    let more = works.len().saturating_sub(origin_meta::MAX_TITLES);
    if more > 0 {
        titles.push(format!("{}{more}", origin_meta::MORE_PREFIX));
    }
    fields.insert(
        keys::ORIGIN_META.to_owned(),
        json!({
            origin_meta::BACKGROUND_SOURCE: work_kind(first.kind).as_str(),
            origin_meta::WORK_ID: first.id,
            origin_meta::TITLE: titles.join(origin_meta::TITLE_SEPARATOR),
        }),
    );
}

/// 回合的来源 → `turnHeader.origin`（session-lifecycle.md §10.8 的换算）。
pub(crate) fn origin(origin: TurnOrigin) -> &'static str {
    match origin {
        TurnOrigin::Input => USER_ORIGIN,
        TurnOrigin::Notice => "backgroundResult",
        TurnOrigin::Peer => "mailbox",
        TurnOrigin::Scheduled => "automation",
        TurnOrigin::Compaction => "synthetic",
    }
}

/// 已结束的回合的回合头状态（回合记录的结局，session-lifecycle.md §10.17；同活会话 `TurnFinished` 的换算）。
pub(crate) fn ended_state(outcome: InputEnd) -> &'static str {
    match outcome {
        InputEnd::Completed => turn_state::COMPLETED,
        InputEnd::Cancelled => turn_state::CANCELLED,
        InputEnd::Failed => turn_state::ERROR,
    }
}

/// 回合头的字段（`turnHeader`）。
pub(crate) fn turn_header(
    turn: &str,
    seq: u64,
    origin: &str,
    started_at: u64,
    state: &str,
) -> Fields {
    let running = state == turn_state::RUNNING;
    let mut fields = base(kinds::TURN_HEADER, turn, seq, !running, Some(started_at));
    fields.insert(keys::ORIGIN.to_owned(), json!(origin));
    fields.insert(keys::STATE.to_owned(), json!(state));
    fields.insert(keys::STARTED_AT.to_owned(), millis(started_at));
    fields
}

/// `turnHeader.durations` 的两项（protocol-v5.md §29.1）。
mod durations {
    pub(super) const ACTIVE_MS: &str = "activeMs";
    pub(super) const TOTAL_MS: &str = "totalMs";
}

/// 回合头的结束时刻与耗时（protocol-v5.md §29.1）：`activeMs` 是内核扣掉等回答之后的用时，`totalMs` 是墙上时间（结束减回合头的开始）。
pub(crate) fn set_ended(fields: &mut Fields, started_at: u64, time: TurnTime) {
    fields.insert(keys::ENDED_AT.to_owned(), millis(time.ended_at));
    fields.insert(
        keys::DURATIONS.to_owned(),
        json!({
            durations::ACTIVE_MS: millis(time.active_ms),
            durations::TOTAL_MS: millis(time.ended_at.saturating_sub(started_at)),
        }),
    );
}

/// 用户输入行（`userInput`）。
pub(crate) fn user_input(
    text: &str,
    guided: bool,
    turn: &str,
    seq: u64,
    created_at: Option<u64>,
) -> Fields {
    let mut fields = base(kinds::USER_INPUT, turn, seq, true, created_at);
    fields.insert(keys::TEXT.to_owned(), json!(text));
    fields.insert(keys::ORIGIN.to_owned(), json!(USER_ORIGIN));
    fields.insert(keys::GUIDED.to_owned(), json!(guided));
    fields
}

/// 正文行（`assistantText`）或思考行（`reasoning`）：`state` 是 streaming 或 done。
pub(crate) fn answer(
    kind: &str,
    text: &str,
    state: &str,
    message: &str,
    (turn, seq, created_at): (&str, u64, Option<u64>),
) -> Fields {
    let mut fields = base(kind, turn, seq, state == DONE, created_at);
    fields.insert(keys::TEXT.to_owned(), json!(text));
    fields.insert(keys::STATE.to_owned(), json!(state));
    fields.insert(keys::RESPONSE.to_owned(), json!(message));
    fields
}

/// 工具调用行（`toolCall`）。
pub(crate) fn tool_call(
    status: &str,
    message: &str,
    (turn, seq, created_at): (&str, u64, Option<u64>),
) -> Fields {
    let mut fields = base(
        kinds::TOOL_CALL,
        turn,
        seq,
        tool_status::ended(status),
        created_at,
    );
    fields.insert(keys::STATUS.to_owned(), json!(status));
    fields.insert(keys::TOOL_NAME.to_owned(), json!(""));
    fields.insert(keys::INPUT_TEXT.to_owned(), json!(""));
    fields.insert(keys::RESPONSE.to_owned(), json!(message));
    fields
}

/// 工具行换状态（`stable` 跟着：有结局才稳定）。
pub(crate) fn set_status(fields: &mut Fields, status: &str) {
    fields.insert(keys::STATUS.to_owned(), json!(status));
    fields.insert(keys::STABLE.to_owned(), json!(tool_status::ended(status)));
}

/// 工具的结果：`output{text, truncated?, display?}`（v4 `toolOutputSchema` 的形状，大输出只带预览，01 §19.5）；失败时另有 `error` 文字。`display` 是只给界面的数据
/// （§21：活会话里 `ToolDone` 带来）；为 None 时留着行上已有的那一份——结果消息排在 `ToolDone` 之后，只换文字。
pub(crate) fn set_result(fields: &mut Fields, text: &str, is_error: bool, display: Option<Value>) {
    set_status(
        fields,
        if is_error {
            tool_status::ERROR
        } else {
            tool_status::COMPLETED
        },
    );
    let display = display.or_else(|| {
        fields
            .get(keys::OUTPUT)
            .and_then(|output| output.get(keys::DISPLAY))
            .cloned()
    });
    // 行上只带预览（01 §19.5）：大输出只有开头与结尾，全文走 `tool.output`；出错时的 `error` 是同一段开头
    let (mut output, shown) = super::tool_output::preview(text);
    if let Some(display) = display {
        output[keys::DISPLAY] = display;
    }
    fields.insert(keys::OUTPUT.to_owned(), output);
    if is_error {
        fields.insert(keys::ERROR.to_owned(), json!(shown));
    }
}

/// 压缩标记行（`timelineMarker`）：`detail` 是契约的 `CompactMarkerDetail`（02 §11.8 第 4 项，v4 的 compact marker 形状）。在跑的标记还会变，
/// 不稳定（§15.3.4）。
fn compact_marker(
    turn: &str,
    seq: u64,
    created_at: Option<u64>,
    detail: &CompactMarkerDetail,
) -> Fields {
    let stable = detail.status != CompactMarkerDetailStatus::Running;
    let mut fields = base(kinds::TIMELINE_MARKER, turn, seq, stable, created_at);
    fields.insert(keys::MARKER.to_owned(), json!(COMPACT_MARKER));
    fields.insert(keys::DETAIL.to_owned(), json!(detail));
    fields
}

/// 压缩成功的标记：`detail` 取自边界消息，冷读与活会话同值。
///
/// 修 bug（19 §10 审定第 6 条，v5 的 `/compact`）：以前没有 `detail`，UI 按 v4 的 marker 读它、读不出就跳过这一行，压缩之后时间线上看不到标记。
pub(crate) fn marker(
    turn: &str,
    seq: u64,
    created_at: Option<u64>,
    trigger: CompactTrigger,
    tokens_before: u64,
) -> Fields {
    let detail = CompactMarkerDetail {
        r#type: CompactMarkerDetailType::Compact,
        origin: marker_origin(trigger),
        status: CompactMarkerDetailStatus::Success,
        tokens_before: Some(tokens_before),
    };
    compact_marker(turn, seq, created_at, &detail)
}

/// 压缩标记的来源（照 v4 的 `mapCompactMarkerOrigin`）：手动之外都是 auto。
pub(crate) fn marker_origin(trigger: CompactTrigger) -> CompactMarkerDetailOrigin {
    match trigger {
        CompactTrigger::Manual => CompactMarkerDetailOrigin::Manual,
        CompactTrigger::Auto | CompactTrigger::Reactive => CompactMarkerDetailOrigin::Auto,
    }
}

/// 压缩的回合在跑、失败与被停止时的标记的行 id（§23.1、§47.1、§50.1）。
pub(crate) fn turn_compaction_id(turn: &str) -> String {
    format!("{turn}{TURN_COMPACTION_SUFFIX}")
}

/// 主循环里的一次压缩的标记的行 id（§50.2）：`index` 是它在回合记录的 `compactions` 里的位置（一轮可以压缩几次）。
pub(crate) fn loop_compaction_id(turn: &str, index: usize) -> String {
    format!("{turn}{TURN_COMPACTION_SUFFIX}:{index}")
}

/// 压缩在跑（`Running`）、失败（`Failed`）或被停止（`Cancelled`）时的标记（02 §11.8 第 4、5 项，§47.1、§50）：同一行，开始时出、结束时原地改；
/// 活会话与冷读按回合的开始与回合记录建同一行。压缩的回合（`/compact`，来源手动）的 `created_at` 是回合开始的时刻；主循环里的压缩（来源 auto）
/// 是它开始的时刻。
///
/// 修 bug（15 §14.3）：以前压缩失败时时间线上没有标记，只有横幅里的原文；v4 有失败的标记。
pub(crate) fn pending_compaction(
    turn: &str,
    seq: u64,
    created_at: Option<u64>,
    origin: CompactMarkerDetailOrigin,
    status: CompactMarkerDetailStatus,
) -> Fields {
    let detail = CompactMarkerDetail {
        r#type: CompactMarkerDetailType::Compact,
        origin,
        status,
        tokens_before: None,
    };
    compact_marker(turn, seq, created_at, &detail)
}

/// 输入行的高速卡（protocol-v5.md §46.1）：注记原样，有退回时再放这一轮的退回时刻与原因。活会话与由视图建同一个函数。
pub(crate) fn set_highspeed(
    fields: &mut Fields,
    note: &Value,
    fell_back: Option<&rust_cli_kernel_api::turn_execution::FellBack>,
) {
    let mut card = note.clone();
    if let (Some(fell_back), Some(card)) = (fell_back, card.as_object_mut()) {
        card.insert(fallback::AT.to_owned(), json!(fell_back.at));
        card.insert(fallback::REASON.to_owned(), json!(fell_back.reason));
    }
    fields.insert(keys::HIGHSPEED.to_owned(), card);
}

/// 产生这一行的命令（protocol-v5.md §24.1）：有才写，没有不猜。
pub(crate) fn set_command(fields: &mut Fields, command: Option<&CommandId>) {
    if let Some(command) = command {
        fields.insert(keys::SOURCE_COMMAND_ID.to_owned(), json!(command.as_str()));
    }
}

/// 字段里的字符串（读回已发布的行）。
/// 真实用户输入（`userInput` 的来源是用户，不是后台结果、目标续写、信箱、合成、工作流启动）：问题导航的粒度（§73.5）。
pub(crate) fn real_user(fields: &Fields) -> bool {
    text(fields, keys::ORIGIN) == USER_ORIGIN
}

pub(crate) fn text<'a>(fields: &'a Fields, key: &str) -> &'a str {
    fields.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
