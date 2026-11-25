//! 主线程请求的已定前缀（docs/specs/rust-cli/request-prefix.md）：按会话的表、校验、从切点接着规范化、交给模型端口的计划。
//!
//! 表里的条目只是派生的缓存：对不上就整段规范化，结果相同（§5）。条目在准备请求时取出，模型端口编码之后经计划放回（§3、§7）。

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::history;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::ApiMessage;
use rust_cli_kernel_api::request_prefix::{PlanRef, PrefixPlan, WirePrefix};
use rust_cli_kernel_api::store::ArtifactStore;
use rust_cli_prompts::models::ToolSearchMode;

use crate::deferred::tool_references;
use crate::normalize::{NormalizeOptions, Resume, normalize, normalize_from};

/// 每个会话一条已定前缀（request-prefix.md §3）。runtime 持有，会话关闭时释放（[`Self::forget`]）；主循环在准备主线程的
/// 请求时取出，模型端口编码之后经计划放回。
#[derive(Default)]
pub struct RequestPrefixes {
    entries: Mutex<HashMap<SessionId, Settled>>,
}

impl RequestPrefixes {
    /// 会话关闭：丢掉它的条目。
    pub fn forget(&self, session: &SessionId) {
        self.entries.lock_unpoisoned().remove(session);
    }

    fn take(&self, session: &SessionId) -> Option<Settled> {
        self.entries.lock_unpoisoned().remove(session)
    }

    fn put(&self, session: SessionId, settled: Settled) {
        self.entries.lock_unpoisoned().insert(session, settled);
    }
}

/// 一条已定前缀：源前缀（活动历史前几条的 id）、键、接续状态与模型端口写好的线编码（§3）。
struct Settled {
    ids: Vec<MessageId>,
    key: Key,
    resume: Resume,
    wire: WirePrefix,
}

/// 规范化的输入里除了消息之外会改变输出的部分（§5）：对话中间的 system、每轮 effort、ToolSearch 的形式与可用工具名
/// （`tool_reference` 按它们改写）。
#[derive(Debug, Clone, PartialEq)]
struct Key {
    options: (bool, Option<String>),
    tool_search: ToolSearchMode,
    tool_names: Vec<String>,
}

/// 请求要发的源消息：活动历史按 ToolSearch 的形式与可用工具名改写 `tool_reference`（与整段时相同，§4 的表）。
pub(crate) fn sources<'a>(
    active: &'a [Cow<'a, Message>],
    mode: ToolSearchMode,
    tool_names: &[String],
) -> Vec<Cow<'a, Message>> {
    active
        .iter()
        .map(|message| tool_references(message, mode, tool_names))
        .collect()
}

/// 整段重建要的输入（§6：编码身份对不上时模型端口经计划要整段）。持有主循环历史的一份引用，只到编码结束。
#[derive(Clone)]
struct Rebuild {
    history: Arc<Vec<Message>>,
    tool_search: ToolSearchMode,
    tool_names: Vec<String>,
    options: NormalizeOptions,
    artifacts: Arc<dyn ArtifactStore>,
}

impl Rebuild {
    /// 与没有前缀时主循环发出的消息相同：活动历史、`tool_reference` 改写、整段规范化、物化。
    async fn messages(self) -> Vec<ApiMessage> {
        let active = history::active(&self.history);
        let sources = sources(&active, self.tool_search, &self.tool_names);
        let mut messages = normalize(sources.iter().map(AsRef::as_ref), &self.options);
        crate::materialize::materialize(&mut messages, self.artifacts.as_ref()).await;
        messages
    }
}

/// 编码之后要放回的条目（没有线编码的部分）。
struct Next {
    table: Arc<RequestPrefixes>,
    session: SessionId,
    ids: Vec<MessageId>,
    key: Key,
    resume: Resume,
}

struct Plan {
    wire: Option<WirePrefix>,
    settle: usize,
    rebuild: Rebuild,
    next: Mutex<Option<Next>>,
}

impl PrefixPlan for Plan {
    fn wire(&self) -> Option<&WirePrefix> {
        self.wire.as_ref()
    }

    fn settle(&self) -> usize {
        self.settle
    }

    fn full(&self) -> BoxFuture<'static, Vec<ApiMessage>> {
        self.rebuild.clone().messages().boxed()
    }

    fn keep(&self, prefix: WirePrefix) {
        let Some(next) = self.next.lock_unpoisoned().take() else {
            return;
        };
        next.table.put(
            next.session,
            Settled {
                ids: next.ids,
                key: next.key,
                resume: next.resume,
                wire: prefix,
            },
        );
    }
}

/// 这一步的输入（§3）：会话、表、主循环的历史、ToolSearch 的形式与可用工具名、规范化选项、Artifact 存储。
pub(crate) struct StepInput<'a> {
    pub(crate) session: &'a SessionId,
    pub(crate) table: &'a Arc<RequestPrefixes>,
    pub(crate) history: &'a Arc<Vec<Message>>,
    pub(crate) tool_search: ToolSearchMode,
    pub(crate) tool_names: &'a [String],
    pub(crate) options: &'a NormalizeOptions,
    pub(crate) artifacts: &'a Arc<dyn ArtifactStore>,
}

/// 一步的消息与计划：条目对得上时只规范化切点之后的源消息，`messages` 是前缀之后的那一段；对不上时整段。两种情形都记下
/// 这一步的最后一个切点，编码之后由模型端口经计划放回（§3）。
pub(crate) async fn messages(
    input: StepInput<'_>,
    sources: &[&Message],
) -> (Vec<ApiMessage>, PlanRef) {
    let key = Key {
        options: (
            input.options.system_turns,
            input.options.turn_effort.clone(),
        ),
        tool_search: input.tool_search,
        tool_names: input.tool_names.to_vec(),
    };
    // 校验（§5）：键相同，源前缀的 id 按顺序相同，历史比切点长（切点那条 assistant 还在）。
    let hit = input.table.take(input.session).filter(|settled| {
        settled.key == key
            && settled.ids.len() < sources.len()
            && settled
                .ids
                .iter()
                .zip(sources)
                .all(|(id, message)| id == message.id())
    });
    let (start, resume, wire, mut ids) = match hit {
        Some(settled) => (
            settled.ids.len(),
            settled.resume,
            Some(settled.wire),
            settled.ids,
        ),
        None => (0, Resume::default(), None, Vec::new()),
    };
    let (mut messages, cut) = normalize_from(&sources[start..], input.options, resume, true);
    crate::materialize::materialize(&mut messages, input.artifacts.as_ref()).await;
    let (settle, next) = match cut {
        Some(cut) => {
            let end = start + cut.source;
            ids.extend(
                sources[start..end]
                    .iter()
                    .map(|message| message.id().clone()),
            );
            let next = Next {
                table: input.table.clone(),
                session: input.session.clone(),
                ids,
                key,
                resume: cut.resume,
            };
            (cut.settled, Some(next))
        }
        None => (0, None),
    };
    let plan = Plan {
        wire,
        settle,
        rebuild: Rebuild {
            history: input.history.clone(),
            tool_search: input.tool_search,
            tool_names: input.tool_names.to_vec(),
            options: input.options.clone(),
            artifacts: input.artifacts.clone(),
        },
        next: Mutex::new(next),
    };
    (messages, PlanRef(Arc::new(plan)))
}

#[cfg(test)]
#[path = "prefix_tests.rs"]
mod tests;
