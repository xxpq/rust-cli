//! 会话消息 → 发给模型的 API 消息（规则见 docs/specs/rust-cli/context.md §3）。
//!
//! 1. attachment 上浮：排到它前面的人类输入之前，直到遇到 assistant 或工具结果；
//! 2. user 角色的 attachment 并进相邻的 user 消息；system 角色的 attachment 去掉
//!    `<system-reminder>` 包装后用空行连接，在下一条 assistant 之前或末尾折叠成一条
//!    `role: system` 消息（模型不支持时按 user 角色处理）；带内容块的 attachment（图片等）不折叠，
//!    按 user 处理（折叠只收纯文字）；
//! 3. 相邻 user 消息合并时，前一条以文本结尾、后一条以文本开头就在前一段文本后补一个换行，
//!    工具结果排在最前；按 user 角色接上的提醒遇到末尾的工具结果时并进它（kernel-api `reminder_fold`）；
//! 4. 人类插话（`queued_command`、`commandMode: prompt`）在有 system turn 时按人类输入：去掉包装，作为文本块接在上一条
//!    user 消息（工具结果）后面（context.md §3 第 10 条）；
//! 5. 每轮 effort：人类输入之后的 effort 与上次说过的不同时，挂在
//!    紧跟其后的 system 消息上（没有就插一条只带 `output_config` 的 system 消息）；
//! 6. 最后一条有内容的消息的最后一个块打缓存断点（默认计划）；摘要请求
//!    （不写缓存）往前挪一条；
//! 7. 每条 assistant 里 tool_use 移到末尾（context.md §3 第 11 条），合并之后做配对兜底（第 12 条，`pairing`）。
//!
//! 其余规则（tool_reference 剥离、thinking 清理、错误结果去图等）属于工作线 L1。

use std::collections::{HashMap, HashSet};

use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{AttachmentMessage, AttachmentRole, ContentBlock, Message};
use rust_cli_kernel_api::model::{ApiContent, ApiMessage, ApiRole, CacheControl, TurnOutputConfig};
use rust_cli_kernel_api::reminder_fold::{array_result, fold_into_tool_result};
use rust_cli_prompts::reminder;
use serde_json::Value;

/// normalize 的选项（随模型变化）。
#[derive(Debug, Clone, Default)]
pub(crate) struct NormalizeOptions {
    /// 模型接受对话中间的 `role: system` 消息。
    pub system_turns: bool,
    /// 模型接受每轮的 effort（beta `per-turn-control`）时，这次请求的 effort。
    pub turn_effort: Option<String>,
    /// 摘要请求不写缓存：断点往前挪一条（[`skip_write_marker`]）。
    pub skip_cache_write: bool,
}

fn message(role: ApiRole, content: Vec<ApiContent>) -> ApiMessage {
    ApiMessage {
        role,
        content,
        output_config: None,
    }
}

fn plain(blocks: Vec<ContentBlock>) -> Vec<ApiContent> {
    blocks
        .into_iter()
        .map(|block| ApiContent {
            block,
            cache_control: None,
        })
        .collect()
}

fn text(value: String) -> ApiContent {
    ApiContent {
        block: ContentBlock::Text { text: value },
        cache_control: None,
    }
}

/// 人类插话（记录里带 `humanTurn: true` 的 `queued_command`）：插话的 `commandMode` 是 `prompt`，
/// 后台通知是 `task-notification`（query.md §4、§5）。
fn is_human_prompt(attachment: &AttachmentMessage) -> bool {
    attachment.source == queued_command::SOURCE
        && attachment
            .data
            .get(queued_command::MODE_KEY)
            .and_then(Value::as_str)
            == Some(queued_command::PROMPT_MODE)
}

/// 停止上浮的位置：assistant，或以工具结果开头的 user 消息。
fn stops_hoisting(message: &Message) -> bool {
    match message {
        Message::Assistant(_) => true,
        Message::User(user) => {
            matches!(user.content.first(), Some(ContentBlock::ToolResult { .. }))
        }
        Message::Attachment(_) | Message::System(_) => false,
    }
}

/// attachment 上浮：从后往前扫，attachment 越过普通 user 消息，
/// 停在最近的 assistant / 工具结果之后。每一项带着它在输入里的下标（切点按它记，request-prefix.md §4）。
fn hoist(messages: Vec<(usize, &Message)>) -> Vec<(usize, &Message)> {
    if !messages
        .iter()
        .any(|(_, message)| matches!(message, Message::Attachment(_)))
    {
        return messages;
    }
    let mut reversed: Vec<(usize, &Message)> = Vec::with_capacity(messages.len());
    let mut pending: Vec<(usize, &Message)> = Vec::new();
    for item in messages.into_iter().rev() {
        if matches!(item.1, Message::Attachment(_)) {
            pending.push(item);
            continue;
        }
        if stops_hoisting(item.1) {
            reversed.append(&mut pending);
        }
        reversed.push(item);
    }
    reversed.append(&mut pending);
    reversed.reverse();
    reversed
}

/// 工具结果排在最前，其余保持原序。
fn tool_results_first(content: &mut Vec<ApiContent>) {
    let (mut results, rest): (Vec<_>, Vec<_>) = content
        .drain(..)
        .partition(|item| matches!(item.block, ContentBlock::ToolResult { .. }));
    results.extend(rest);
    *content = results;
}

/// 相邻 user 消息合并：文本接文本时，前一段末尾补一个换行。
fn merge_user(into: &mut Vec<ApiContent>, next: Vec<ApiContent>) {
    if let (Some(last), Some(first)) = (into.last_mut(), next.first())
        && let (ContentBlock::Text { text }, ContentBlock::Text { .. }) =
            (&mut last.block, &first.block)
    {
        text.push('\n');
    }
    into.extend(next);
    tool_results_first(into);
}

struct Builder {
    out: Vec<ApiMessage>,
    folded: Vec<String>,
    /// 已发出的 tool_use 的工具名：提醒并进工具结果时按它推结果的形式（`array_result`）。
    tool_names: HashMap<ToolUseId, String>,
    /// 记切点时（[`CutPoint`]）：最后一个切点之后 `tool_names` 的改动（键与改动前的值），结束时撤回就是切点处的表。
    undo: Option<Vec<(ToolUseId, Option<String>)>>,
    /// 最后一个切点：那条 assistant 在输入里的下标与它之前的输出条数。
    cut: Option<(usize, usize)>,
}

impl Builder {
    fn last_role(&self) -> Option<ApiRole> {
        self.out.last().map(|message| message.role)
    }

    fn push_user(&mut self, content: Vec<ApiContent>) {
        match self.out.last_mut() {
            Some(last) if last.role == ApiRole::User => merge_user(&mut last.content, content),
            _ => self.out.push(message(ApiRole::User, content)),
        }
    }

    /// user 角色的 attachment：接在上一条 user 消息之后，不补换行；那条消息
    /// 以工具结果结尾时并进工具结果（context.md §3 第 9 条）。
    ///
    /// 缺陷修复（R4 F7）：原来总是另起文本块。模型不支持 system turn 时每个工具轮的提醒都走这里，
    /// 应把它并进工具结果的字符串、缓存断点落在工具结果上，所以 GLM 等没有卡片的模型每个工具轮的请求形状都不对。
    fn push_reminder(&mut self, value: String) {
        match self.out.last_mut() {
            Some(last) if last.role == ApiRole::User => {
                let names = &self.tool_names;
                let array_form = |id: &ToolUseId, is_error: bool| {
                    names
                        .get(id)
                        .is_some_and(|name| array_result(name, is_error))
                };
                if !fold_into_tool_result(&mut last.content, &value, array_form) {
                    last.content.push(text(value));
                    tool_results_first(&mut last.content);
                }
            }
            _ => self.out.push(message(ApiRole::User, vec![text(value)])),
        }
    }

    /// 带内容块的 attachment（docs/specs/rust-cli/tui-input.md §1.3，context.md §3 第 8 条）：上一条是 user 时，
    /// 调用说明与内容块依次接在后面；否则调用说明单独一条 user 消息、内容块另起一条（这两条 meta
    /// 消息原样推入，之后的内容接在第二条后面）。
    fn push_media(&mut self, value: String, blocks: Vec<ContentBlock>) {
        let blocks = plain(blocks);
        match self.out.last_mut() {
            Some(last) if last.role == ApiRole::User => {
                last.content.push(text(value));
                last.content.extend(blocks);
                tool_results_first(&mut last.content);
            }
            _ => {
                self.out.push(message(ApiRole::User, vec![text(value)]));
                self.out.push(message(ApiRole::User, blocks));
            }
        }
    }

    /// 折叠的 system 文本落地。
    fn flush(&mut self) {
        if self.folded.is_empty() {
            return;
        }
        let joined = self.folded.drain(..).collect::<Vec<_>>().join("\n\n");
        match self.last_role() {
            Some(ApiRole::System) => {
                if let Some(ContentBlock::Text { text }) = self
                    .out
                    .last_mut()
                    .and_then(|last| last.content.last_mut())
                    .map(|item| &mut item.block)
                {
                    text.push_str("\n\n");
                    text.push_str(&joined);
                }
            }
            Some(ApiRole::User) => self.out.push(message(ApiRole::System, vec![text(joined)])),
            // 前面没有 user 消息可跟：退回成一条带包装的 user 提醒。
            Some(ApiRole::Assistant) | None => self
                .out
                .push(message(ApiRole::User, vec![text(reminder::wrap(&joined))])),
        }
    }
}

fn is_thinking(block: &ContentBlock) -> bool {
    matches!(
        block,
        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
    )
}

/// 回复里 tool_use 之后还有别的块时，tool_use 移到末尾，其余块保持相对顺序、各自分开；有 tool_use 段夹在两个
/// thinking 块之间时整条不动（query.md §1.7）。
///
/// 修 bug（工作线 F81）：以前实时路径按 API 的块顺序发，冷读取却是文字在前、tool_use 在后，同一段历史续写前后
/// 请求不同。
fn tool_uses_last(blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    let is_call = |block: &ContentBlock| matches!(block, ContentBlock::ToolUse { .. });
    let (mut seen_call, mut after_call, mut splits_thinking) = (false, false, false);
    let (mut previous_thinking, mut in_calls) = (false, false);
    for block in &blocks {
        if is_call(block) {
            seen_call = true;
            in_calls = true;
            continue;
        }
        after_call |= seen_call;
        let thinking = is_thinking(block);
        splits_thinking |= thinking && previous_thinking && in_calls;
        previous_thinking = thinking;
        in_calls = false;
    }
    if !after_call || splits_thinking {
        return blocks;
    }
    let (calls, mut rest): (Vec<_>, Vec<_>) = blocks.into_iter().partition(is_call);
    rest.extend(calls);
    rest
}

/// 每轮 effort 的扫描状态（request-prefix.md §4）：说过没有、上一条 user 之后等着挂。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Effort {
    announced: bool,
    pending: bool,
}

/// 每轮 effort。assistant 消息还没有记录当时的 effort
/// （工作线 L1），所以每轮都按这次请求的 effort 算：只有第一条人类输入之后会带上它。
/// `state` 是接着扫的状态；`split` 给出时另返回处理第 `split` 条之前的输出条数与状态（切点，request-prefix.md §4）。
fn turn_effort(
    messages: Vec<ApiMessage>,
    effort: &str,
    mut state: Effort,
    split: Option<usize>,
) -> (Vec<ApiMessage>, Option<(usize, Effort)>) {
    let config = || TurnOutputConfig {
        effort: Some(effort.to_owned()),
    };
    let mut out = Vec::with_capacity(messages.len() + 1);
    let mut at_split = None;
    for (index, mut item) in messages.into_iter().enumerate() {
        if split == Some(index) {
            at_split = Some((out.len(), state));
        }
        if state.pending && item.role != ApiRole::User {
            state.pending = false;
            state.announced = true;
            if item.role == ApiRole::System {
                item.output_config = Some(config());
                out.push(item);
                continue;
            }
            out.push(ApiMessage {
                output_config: Some(config()),
                ..message(ApiRole::System, Vec::new())
            });
        }
        let is_user = item.role == ApiRole::User;
        out.push(item);
        if is_user {
            state.pending = !state.announced;
        }
    }
    if state.pending {
        out.push(ApiMessage {
            output_config: Some(config()),
            ..message(ApiRole::System, Vec::new())
        });
    }
    (out, at_split)
}

/// 从切点接着规范化的状态（request-prefix.md §4）：builder 的工具名、配对见过的 tool_use、每轮 effort 的扫描状态。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Resume {
    tool_names: HashMap<ToolUseId, String>,
    seen: HashSet<ToolUseId>,
    effort: Effort,
}

/// 一次规范化里最后一个切点（request-prefix.md §4）：一条 assistant，builder 处理到它时输出的最后一条不是 assistant。
/// 它之前的输出之后不再变，下一步从它接着规范化。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CutPoint {
    /// 那条 assistant 在输入里的下标。
    pub(crate) source: usize,
    /// 它之前的输出条数。
    pub(crate) settled: usize,
    /// 从它接着规范化的状态。
    pub(crate) resume: Resume,
}

pub(crate) fn normalize<'a>(
    messages: impl IntoIterator<Item = &'a Message>,
    options: &NormalizeOptions,
) -> Vec<ApiMessage> {
    let messages: Vec<&Message> = messages.into_iter().collect();
    normalize_from(&messages, options, Resume::default(), false).0
}

/// 规范化。`resume` 是接着规范化的状态（从头时是缺省值；接着切点时 `messages` 从切点那条 assistant 开始，request-prefix.md §4）；
/// `track` 时另给出最后一个切点。输出与把切点之前的消息一起整段规范化、再去掉切点之前的输出相同。
pub(crate) fn normalize_from(
    messages: &[&Message],
    options: &NormalizeOptions,
    resume: Resume,
    track: bool,
) -> (Vec<ApiMessage>, Option<CutPoint>) {
    let Resume {
        tool_names,
        seen,
        effort,
    } = resume;
    let mut builder = Builder {
        out: Vec::new(),
        folded: Vec::new(),
        tool_names,
        undo: track.then(Vec::new),
        cut: None,
    };
    for (source, item) in hoist(messages.iter().copied().enumerate().collect()) {
        match item {
            // 只有附件、没有正文的输入没有内容块（session-lifecycle.md §10.14）：不发空的 user 消息，跟在后面的附件接成那一条。
            Message::User(user) if user.content.is_empty() => {}
            Message::User(user) => builder.push_user(plain(user.content.clone())),
            // 只供显示的 attachment（如 `max_turns_reached`）文本为空，渲染为空。
            Message::Attachment(attachment) if attachment.text.is_empty() => {}
            // 缺陷修复（修复线 F13）：人类插话原来按 system 角色折进末尾的 system 消息。应在
            // 有 system turn 时把它当人类输入（缺省开）：去包装、接在工具结果所在的
            // user 消息后面（先于 system 文字落地）。没有 system turn 时走下面的老路。
            Message::Attachment(attachment)
                if options.system_turns && is_human_prompt(attachment) =>
            {
                builder.push_user(vec![text(reminder::unwrap(&attachment.text).to_owned())]);
            }
            Message::Attachment(attachment) => {
                let blocks = attachment.blocks();
                if !blocks.is_empty() {
                    builder.push_media(attachment.text.clone(), blocks);
                } else if options.system_turns && attachment.role() == AttachmentRole::System {
                    builder
                        .folded
                        .push(reminder::unwrap(&attachment.text).to_owned());
                } else {
                    builder.push_reminder(attachment.text.clone());
                }
            }
            Message::Assistant(assistant) => {
                builder.flush();
                // 切点：这条 assistant 新起一条输出，之前的输出之后不再改（request-prefix.md §4）。
                if let Some(undo) = &mut builder.undo
                    && builder.out.last().map(|last| last.role) != Some(ApiRole::Assistant)
                {
                    builder.cut = Some((source, builder.out.len()));
                    undo.clear();
                }
                for block in &assistant.content {
                    if let ContentBlock::ToolUse { id, name, .. } = block {
                        let previous = builder.tool_names.insert(id.clone(), name.clone());
                        if let Some(undo) = &mut builder.undo {
                            undo.push((id.clone(), previous));
                        }
                    }
                }
                let content = plain(tool_uses_last(assistant.content.clone()));
                match builder.out.last_mut() {
                    Some(last) if last.role == ApiRole::Assistant => last.content.extend(content),
                    _ => builder.out.push(message(ApiRole::Assistant, content)),
                }
            }
            Message::System(_) => {}
        }
    }
    builder.flush();
    let Builder {
        out,
        mut tool_names,
        undo,
        cut,
        ..
    } = builder;
    let (paired, paired_cut) = crate::pairing::ensure_pairing_from(out, seen, cut.map(|cut| cut.1));
    let (mut out, effort_cut) = match &options.turn_effort {
        Some(value) => turn_effort(paired, value, effort, paired_cut.as_ref().map(|cut| cut.0)),
        None => {
            let at = paired_cut.as_ref().map(|cut| (cut.0, effort));
            (paired, at)
        }
    };
    let marker = if options.skip_cache_write {
        skip_write_marker(&mut out)
    } else {
        out.iter_mut()
            .rev()
            .find_map(|item| item.content.last_mut())
    };
    if let Some(last) = marker {
        last.cache_control = Some(CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: None,
        });
    }
    let cut = match (cut, paired_cut, effort_cut, undo) {
        (Some((source, _)), Some((_, seen)), Some((settled, effort)), Some(undo)) => {
            // 撤回最后一个切点之后的工具名改动，得到切点处的表。
            for (id, previous) in undo.into_iter().rev() {
                match previous {
                    Some(name) => tool_names.insert(id, name),
                    None => tool_names.remove(&id),
                };
            }
            Some(CutPoint {
                source,
                settled,
                resume: Resume {
                    tool_names,
                    seen,
                    effort,
                },
            })
        }
        _ => None,
    };
    (out, cut)
}

/// 能盖缓存章的消息：不是 system 消息；assistant 的最后一块不是
/// thinking，也不是空文本。
fn stampable(message: &ApiMessage) -> bool {
    match message.role {
        ApiRole::System => false,
        ApiRole::User => true,
        ApiRole::Assistant => message
            .content
            .last()
            .is_some_and(|item| match &item.block {
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => false,
                ContentBlock::Text { text } => !text.is_empty(),
                _ => true,
            }),
    }
}

/// 摘要请求的断点（不写缓存）：最后一条能盖章的消息
/// 之前的那一条的最后一块。
///
/// 缺陷修复（L25）：原来摘要请求与主循环一样标在最后一块（末尾的 system 提醒）上；摘要
/// 请求不写缓存，断点落在倒数第二条能盖章的消息上。
fn skip_write_marker(messages: &mut [ApiMessage]) -> Option<&mut ApiContent> {
    let last = messages.iter().rposition(stampable)?;
    let before = messages[..last].iter().rposition(stampable)?;
    messages[before].content.last_mut()
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "normalize_order_tests.rs"]
mod order_tests;
