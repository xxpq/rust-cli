//! fork（`Command::Fork`，docs/specs/rust-cli/session-lifecycle.md §6.1；F75 起只有 `--fork-session` 用）：新会话行 + 复制活动分支。
//!
//! Node 的 message id 全库唯一，所以复制时换新 ID，并映射引用了消息 ID 的字段（工具结果的
//! assistant、压缩边界的摘要与保留段）；tool_use id 不变。文件不动，checkpoint 不复制。

use std::collections::HashMap;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::ids::{CommandId, MessageId, ToolUseId};
use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::message::{Message, SystemMessage, UserOrigin};
use rust_cli_kernel_api::plan::ShownPlan;
use rust_cli_kernel_api::session_api::{Ack, CommandResult, Forked};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionKind, SessionMeta, TurnRecord};

use crate::actor::{Actor, rejected};

/// Node `titleFromInput` 对空输入的标题。Node 在首条输入时才建会话行，所以原会话标题不会为空；本工程
/// 建会话时就有行，还没有人类输入时标题为空，取同一个缺省值。
const UNTITLED: &str = rust_cli_kernel_api::title::UNTITLED;

/// Node `buildForkedSessionInput` 的标题：`Fork of <原会话标题>`，原样拼接（2026-10-18 用户确认，
/// 取代 `/branch` 的 `<第一条输入> (Branch)`，docs/specs/rust-cli/session-lifecycle.md §6.1）。
pub(crate) fn fork_title(parent: &str) -> String {
    let parent = if parent.is_empty() { UNTITLED } else { parent };
    format!("Fork of {parent}")
}

/// 换新 ID 复制消息，连同新旧 id 的映射。续写时补的 assistant（session-lifecycle.md §6.3.1）不复制：新会话打开时按同一规则再补，
/// 随它的第一批提交落库（F70 的缺陷修复把它从复制里去掉；F77 起它落库时是只给模型看的记录）。
pub(crate) fn copy(
    messages: &[Message],
    ids: &dyn Ids,
) -> (Vec<Message>, HashMap<MessageId, MessageId>) {
    let persisted = || {
        messages.iter().filter(
            |message| !matches!(message, Message::Assistant(assistant) if assistant.is_resume_sentinel()),
        )
    };
    let map: HashMap<MessageId, MessageId> = persisted()
        .map(|m| (m.id().clone(), ids.message()))
        .collect();
    let mapped = |id: &MessageId| map.get(id).cloned().unwrap_or_else(|| id.clone());
    let copied = persisted()
        .map(|message| {
            let mut message = message.clone();
            match &mut message {
                Message::User(user) => {
                    user.id = mapped(&user.id);
                    if let UserOrigin::ToolResult { assistant } = &mut user.origin {
                        *assistant = mapped(assistant);
                    }
                }
                Message::Assistant(assistant) => assistant.id = mapped(&assistant.id),
                Message::Attachment(attachment) => attachment.id = mapped(&attachment.id),
                Message::System(SystemMessage::CompactBoundary(boundary)) => {
                    boundary.id = mapped(&boundary.id);
                    boundary.summary = mapped(&boundary.summary);
                    if let Some(preserved) = &mut boundary.preserved {
                        preserved.head = mapped(&preserved.head);
                        preserved.anchor = mapped(&preserved.anchor);
                        preserved.tail = mapped(&preserved.tail);
                    }
                }
                Message::System(SystemMessage::Interrupted { id }) => *id = mapped(id),
            }
            message
        })
        .collect();
    (copied, map)
}

/// 复制了的消息的回合记录与输入记录（session-lifecycle.md §10.18）：按映射换成新的消息 id；指向没复制的消息的不要。复制了的调用的计划
/// （ExitPlanMode，§10.57）：键是调用 id，复制时不换，原样带上。
fn copied_records(
    actor: &Actor,
    copied: &[Message],
    map: &HashMap<MessageId, MessageId>,
) -> Vec<Fact> {
    let turns = actor.turns.iter().filter_map(|record| {
        let first = map.get(&record.first)?.clone();
        Some(
            TurnRecord {
                first,
                ..record.clone()
            }
            .fact(),
        )
    });
    let inputs = actor.inputs.iter().filter_map(|record| {
        let message = map.get(&record.message)?.clone();
        Some(
            InputRecord {
                message,
                ..record.clone()
            }
            .fact(),
        )
    });
    let calls: std::collections::HashSet<&ToolUseId> =
        copied.iter().flat_map(Message::tool_uses).collect();
    let plans = actor
        .plans
        .iter()
        .filter(|plan| calls.contains(&plan.tool_use))
        .map(ShownPlan::fact);
    turns.chain(inputs).chain(plans).collect()
}

impl Actor {
    pub(crate) async fn fork(&mut self, command: CommandId, at: Option<MessageId>) -> Ack {
        let end = match &at {
            Some(at) => match self.messages.iter().position(|m| m.id() == at) {
                Some(index) => index + 1,
                None => {
                    return rejected(
                        Refusal::NotFound,
                        format!("{at} is not in the conversation"),
                    );
                }
            },
            None => self.messages.len(),
        };
        // 原会话标题的唯一来源是库里的会话行（Node 在 fork 时 `getSession`）：首条输入的标题由存储在
        // 提交时写入，内存里的 `meta.title` 可能还是空的，所以不用它。
        let parent_title = match self.deps.store.load(&self.meta.id).await {
            Ok(loaded) => loaded.meta.title,
            Err(error) => return rejected(Refusal::Failed, error),
        };
        let (copied, map) = copy(&self.messages[..end], self.deps.ids.as_ref());
        // 修 bug（02 §12.7 审定）：以前只复制消息，新会话里那些消息不属于任何回合。回合与输入的记录随消息同一批复制。
        // 修 bug（§10.57）：计划也随它们复制，否则新会话冷读时 ExitPlanMode 行没有计划文字。
        let records = copied_records(self, &copied, &map);
        // 回执写复制到原会话的哪一条、复制了几条（两种 fork 同一种回执，工作线 F134）。
        let through = end
            .checked_sub(1)
            .map(|last| self.messages[last].id().clone());
        let count = copied.len();
        let id = self.deps.ids.session();
        let meta = SessionMeta {
            id: id.clone(),
            kind: SessionKind::Fork,
            parent: Some(self.meta.id.clone()),
            workspace: self.meta.workspace.clone(),
            title: fork_title(&parent_title),
            trace: self.meta.trace.clone(),
            created_at_ms: self.deps.clock.now_ms(),
        };
        let mut facts = vec![
            Fact::ModelSelected {
                selection: self.selection.clone(),
            },
            Fact::ExecutionChanged {
                state: self.execution,
            },
        ];
        facts.extend(
            copied
                .into_iter()
                .map(|message| Fact::MessageAppended { message }),
        );
        facts.extend(records);
        let batch = FactBatch {
            command: Some(command),
            facts,
            turn: None,
        };
        // 建会话与它的事实同一个事务（pg-store.md §3）：失败时不留空会话。
        match self.deps.store.create(meta, batch).await {
            Ok(_) => {
                // 附加目录不落库，fork 出的会话之后由前端另行打开：记下此刻的，它第一次打开时取走（session-dirs.md §12.5）
                self.deps
                    .forks
                    .inherit(id.clone(), self.additional_directories.clone());
                Ack::Accepted {
                    result: CommandResult::Forked(Forked {
                        session: id,
                        through,
                        copied: count,
                    }),
                }
            }
            Err(error) => rejected(Refusal::Failed, error),
        }
    }
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
