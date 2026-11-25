//! 会话存储与 Artifact 端口（docs/specs/rust-cli/README.md §5、§6）。
//!
//! 内核只提交“事实”（[`Fact`]），不知道记录格式。本地适配器 `node-store` 把事实映射成 Node 的
//! message / part / session_entry 记录（逐字节与 Node 一致）；云端适配器映射成自己的表。
//! 一个模型步骤的事实在一次 [`SessionStore::commit`] 里提交，事务成功后才返回回执。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::env::WorkspaceRef;
use crate::ids::{
    AgentId, ArtifactId, CommandId, MessageId, SessionId, ToolUseId, TraceId, TurnId,
};
use crate::message::Message;
use crate::model::ModelSelection;
use crate::permission::PermissionMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Interactive,
    Subagent,
    Fork,
    /// 侧聊（Node `selection_side_chat`，session-lifecycle.md §10.58）：从父会话分出的隐藏会话，不进会话列表。
    SideChat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    pub workspace: WorkspaceRef,
    pub title: String,
    pub trace: TraceId,
    pub created_at_ms: u64,
}

/// 对话分支切点（Node `session.revert`）：活动分支 = `kept` + 切点之后追加的消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertCursor {
    pub target: MessageId,
    pub kept: Vec<MessageId>,
    pub cut_after: MessageId,
    pub generation: u64,
}

/// 一次文件写入的 checkpoint（Node `runtime/workspace_checkpoint`，docs/specs/rust-cli/
/// session-lifecycle.md §4）。写入前后的内容在 `snapshot` 指向的 artifact 里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCheckpoint {
    /// `checkpoint_<uuid>`。
    pub id: String,
    /// 回合开始的那条 user 消息（Node `messageId` / `targetMessageId`）。
    pub prompt: MessageId,
    /// 发出这次工具调用的 assistant 消息（Node `toolMessageId`）。
    pub assistant: MessageId,
    pub tool_use: ToolUseId,
    pub snapshot: ArtifactRef,
    /// 改的是哪台机器上的文件（R3，remote-exec.md §20.7.5、§20.9）；主 env 不写出。额外机器的存成本工程的记录，
    /// Node / App 看不到。
    #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
    pub machine: crate::ids::MachineName,
}

/// 内核提交给存储的一条事实。新增变体时同步更新各存储适配器的映射与 spec §5。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fact {
    /// 延迟落库的会话（草稿，docs/specs/rust-cli/session-facts.md §4）的会话行：只能是一批的第一项，存储先建行再写这一批
    /// 其余的事实，同一个事务。立即落库的会话仍经 [`SessionStore::create`]。
    SessionCreated {
        meta: SessionMeta,
    },
    /// 已接纳输入与它提升成的用户消息同事务提交，本地旧适配器不需要输入账本。
    InputsPromoted {
        commands: Vec<CommandId>,
        message: MessageId,
    },
    MessageAppended {
        message: Message,
    },
    TitleChanged {
        title: String,
    },
    ModelSelected {
        selection: ModelSelection,
    },
    /// 旧的单值写法：权限模式值换模式并关掉 plan，`Plan` 只打开 plan（Node `resolveExecutionState`）。
    ModeChanged {
        mode: PermissionMode,
    },
    /// 执行状态（权限模式 × plan 开关）整体写入（修复线 F20，docs/specs/rust-cli/node-store.md §4.3）。
    ExecutionChanged {
        state: crate::permission::ExecutionState,
    },
    RevertSet {
        cursor: RevertCursor,
    },
    /// 本工程独有的状态，映射成带 `rust-cli/` 前缀的 session_entry（Node 忽略未知类型）。`kind` 取 [`entry_kinds`]。
    Entry {
        kind: String,
        id: String,
        data: Value,
    },
    /// 一次文件写入的 checkpoint；与对应的工具结果在同一批提交。
    CheckpointCreated {
        checkpoint: WorkspaceCheckpoint,
    },
    /// 代码回退已把这些 checkpoint 的写入撤销；之后的冷加载不再给出它们。
    CheckpointsRestored {
        checkpoints: Vec<String>,
    },
    /// 生成的会话标题（docs/specs/rust-cli/titles.md §4）。存储按 Node 的条件写入：当前标题来源仍是
    /// `default` / `first_input` / `generated`；不满足时这一批提交失败，什么都不写。`message` 是首条输入。
    TitleGenerated {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
    },
    /// 用户选“总是允许”时加入本机项目规则集的规则（Node `local_setting(permission/ruleset)`，
    /// docs/specs/rust-cli/shell-permissions.md §5）；与交互的解决在同一批提交。
    PermissionRulesAdded {
        behavior: crate::permission::RuleBehavior,
        rules: Vec<crate::permission::PermissionRule>,
    },
    /// hook 的 `replaceRules` 落在本机（docs/specs/rust-cli/permission-updates.md §2.3）：本机项目规则集里 `behavior` 这一类
    /// 整份换成 `rules`（去重），其余键原样。Node 群协作会话写会话行，同 [`Fact::PermissionRulesAdded`]。
    PermissionRulesReplaced {
        behavior: crate::permission::RuleBehavior,
        rules: Vec<crate::permission::PermissionRule>,
    },
    /// hook 的 `removeRules` 落在本机：本机项目规则集里 `behavior` 这一类按 Node 的去重键去掉 `rules`。
    PermissionRulesRemoved {
        behavior: crate::permission::RuleBehavior,
        rules: Vec<crate::permission::PermissionRule>,
    },
    /// 记住的附加工作目录（`/add-dir` 的「记住」、hook 的 `addDirectories` 落在本机，permission-updates.md §2.3、§6）：
    /// 本机项目规则集的 `additionalDirectories` 追加、去重。总写项目的规则集（群协作会话也是：会话行只有规则与模式）。
    DirectoriesRemembered {
        directories: Vec<crate::env::EnvPath>,
    },
    /// hook 的 `removeDirectories` 落在本机：从本机项目规则集的 `additionalDirectories` 去掉。
    DirectoriesForgotten {
        directories: Vec<crate::env::EnvPath>,
    },
    /// 本机项目的持久模式（Node `local_setting(permission/mode)`，docs/specs/rust-cli/config.md §2.2）：
    /// 用户切换模式时与 `ModeChanged` 同一批提交。`mode` 是 Node 的执行模式（切到 plan 时是切换前的模式）。
    ProjectModeSet {
        mode: crate::permission::PermissionMode,
    },
    /// 一次模型请求的计时与结果（Node `model_usage`，docs/specs/rust-cli/usage.md §2.2）：与这次响应的
    /// assistant 消息同批提交；没有提交消息（失败、丢弃的响应）时单独一批。
    ///
    /// 用量事实（这一个、`ToolCalled` 与 `TurnEnded` 的用量部分）是观测：存储写不进去时只放弃这一行并记日志，同批的
    /// 其他事实照常提交（usage.md §2.5，工作线 F77）。
    ModelCalled {
        call: crate::metering::ModelCall,
    },
    /// 一次工具调用（Node `tool_usage`，usage.md §2.3）：与它的工具结果同批提交。
    ToolCalled {
        call: crate::metering::ToolCall,
    },
    /// 回合结束（Node `turn_usage`，usage.md §2.4）：存储按当前轮的输入消息与用量行算出其余字段。
    TurnEnded {
        status: crate::metering::CallStatus,
    },
    /// 手动 `/compact` 的一次摘要请求（usage.md §2.1.3，工作线 HOOKS）：同 `ModelCalled`，只是 `model_usage` 的轮次是压缩回合
    /// `turn`、父消息为空（Node 的手动压缩是自己的一轮）。会话 actor 在压缩回合里把计量换成它。
    CompactionCalled {
        call: crate::metering::ModelCall,
        turn: TurnId,
    },
    /// 手动 `/compact` 的回合结束（usage.md §2.1.3）：`turn_usage` 写压缩回合一行，没有输入消息，开始是 `started_at_ms`。
    /// 不写会话的 `lastOutcome`（压缩不是真实输入）。
    CompactionEnded {
        turn: TurnId,
        started_at_ms: u64,
        status: crate::metering::CallStatus,
    },
    /// 子代理调用关联到它的子会话（修复线 F2）：Node 的子代理查询（`subagent-session-query.ts`）从 tool part 的
    /// `state.metadata` 读 `childSessionId`、`agentId`、`agentType`。与这次调用的工具结果同批提交，排在结果与它的
    /// checkpoint 之后（[`crate::run_ledger::RunLedger`]）；主会话与子代理（它再派生的子代理）都记。
    SubagentLinked {
        tool_use: ToolUseId,
        agent: AgentId,
        child: SessionId,
        agent_type: String,
    },
    /// 工具结果里只给界面的输出头部（docs/specs/rust-cli/session-lifecycle.md §10.34）：与这次调用的工具结果同批提交，排在结果之后
    /// （[`crate::run_ledger::RunLedger`]）。node-store 写进 tool part 的 `state.metadata.display`（Node 的形状），冷读读回
    /// [`LoadedSession::output_heads`]。
    OutputHead {
        head: crate::command_output::ToolOutputHead,
    },
}

/// 本机项目规则集的一次修改（`SessionStore::change_project_rules`）：`behavior` 这一类里先删 `remove`、再加
/// `add`（Node `applyPermissionUpdates` 的去重规则）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRulesChange {
    pub behavior: crate::permission::RuleBehavior,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<crate::permission::PermissionRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<crate::permission::PermissionRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactBatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandId>,
    pub facts: Vec<Fact>,
    /// 这一批事实所属的当前轮（[`TurnAnchor::after`]）；没有时由存储自己推导（docs/specs/rust-cli/node-store.md §4，
    /// 修复线 F6）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnAnchor>,
}

/// 当前轮：开启它的真实用户输入与轮次 ID（Node `currentUserMessageId` 与 `turnTraceContext.turnId`）。
///
/// 会话状态的 owner（会话 actor、子代理的子会话）持有它并随批次给出：存储写父消息、`anchor.turnId` 与用量行的轮次时
/// 直接用，不再逐行从库里推导（node-store.md §4，修复线 F6，审查 R5-3）。`turn` 为 None 只出现在冷读取里（库里的
/// 输入没有 `anchor.turnId`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAnchor {
    pub prompt: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnId>,
}

impl TurnAnchor {
    /// 提交 `facts` 时的当前轮：批次里正好有一条开启新回合的输入（[`crate::user_input::opens_turn`]）时是它，轮次 ID
    /// 由 `turn` 生成；没有时沿用 `current`；有几条时（复制历史）一个锚点表达不了，为 None，由存储按库推导。
    pub fn after(
        current: Option<&TurnAnchor>,
        facts: &[Fact],
        turn: impl FnOnce() -> TurnId,
    ) -> Option<TurnAnchor> {
        let mut prompts = facts.iter().filter_map(|fact| match fact {
            Fact::MessageAppended {
                message: Message::User(user),
            } if crate::user_input::opens_turn(user) => Some(&user.id),
            _ => None,
        });
        match (prompts.next(), prompts.next()) {
            (None, _) => current.cloned(),
            (Some(prompt), None) => Some(TurnAnchor {
                prompt: prompt.clone(),
                turn: Some(turn()),
            }),
            (Some(_), Some(_)) => None,
        }
    }
}

/// 事务成功后才返回；`sequence` 在一个会话内单调递增。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub sequence: u64,
}

/// 会话历史里的一段（[`StoreQueries::history`]）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HistorySlice {
    pub messages: Vec<Message>,
    /// 开头（`first`）在这一段里的回合记录，按写入顺序。
    pub turns: Vec<TurnRecord>,
    /// 这一段里工具结果的输出头部与工具调用的起止（同 [`LoadedSession`] 的两项，只取这一段的）。
    pub output_heads: Vec<crate::command_output::ToolOutputHead>,
    pub tool_times: Vec<crate::metering::ToolTime>,
}

/// 历史的一段从哪里取（[`StoreQueries::range`]，docs/specs/rust-cli/node-store.md §5.6）。位置都在「显示顺序」里：活动分支上的存储消息，
/// 压缩点之前只供显示的在前，保留段在它原来的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistorySpan {
    /// 存储消息 `floor` 之前（None：从最新起）的至多 `records` 条，段首往前补到它所在回合的开头。
    Before {
        floor: Option<MessageId>,
        records: usize,
    },
    /// 调用过工具 `tool` 的存储消息所在的回合，只取存储消息 `before` 之前的（None：全部）：前端补读下沿之前的计划行用它（node-store.md §5.6）。
    Calling {
        tool: String,
        before: Option<MessageId>,
    },
}

/// 显示顺序里的一段：`[start, end)` 的下标。
pub type SpanRange = std::ops::Range<usize>;

impl HistorySpan {
    /// 在显示顺序 `order`（存储消息的 id，可能重复：回退保留的同一条出现两次）里选出这一段（node-store.md §5.6）：升序、不相交的几段。
    /// `opens(id)`：这条是不是回合记录的开头。回合的范围同建投影时的归属：从一个开头起，到下一个开头之前。第一个记录的开头之前是没有记录的历史，按开回合的
    /// 用户输入切（node-store.md §5.6）：`unrecorded(at)` = 第 `at` 条存储消息开不开这种回合（[`crate::turn_walk::opens_turn`]，调用方解码判定，
    /// 只在这一区域、段首往前找时问）；往前找不到时它自己是开头（会话开头那几条不属于回合）。`floor` 不在 `order` 里时为 None（调用方据此报 NotFound：
    /// 历史已经变了）。
    pub fn ranges<E>(
        &self,
        order: &[MessageId],
        opens: impl Fn(&MessageId) -> bool,
        calls: impl Fn(&MessageId) -> bool,
        unrecorded: impl FnMut(usize) -> Result<bool, E>,
    ) -> Result<Option<Vec<SpanRange>>, E> {
        let starts: Vec<usize> = (0..order.len()).filter(|&at| opens(&order[at])).collect();
        let mut turns = Turns {
            starts,
            len: order.len(),
            unrecorded,
        };
        match self {
            Self::Before { floor, records } => {
                let Some(end) = end_of(order, floor.as_ref()) else {
                    return Ok(None);
                };
                let start = end.saturating_sub(*records);
                let start = if start < end {
                    turns.opening(start)?
                } else {
                    start
                };
                Ok(Some(vec![SpanRange { start, end }]))
            }
            Self::Calling { before, .. } => {
                let Some(end) = end_of(order, before.as_ref()) else {
                    return Ok(None);
                };
                // 这些位置所在的回合，按先后、相交的并成一段
                let mut ranges = Vec::new();
                for at in (0..end).filter(|&at| calls(&order[at])) {
                    ranges.push(turns.opening(at)?..turns.closing(at));
                }
                ranges.sort_by_key(|range| range.start);
                ranges.dedup_by(|later, earlier| {
                    let overlaps = later.start <= earlier.end;
                    if overlaps {
                        earlier.end = earlier.end.max(later.end);
                    }
                    overlaps
                });
                Ok(Some(ranges))
            }
        }
    }
}

/// 段的结尾（不含）：`id` 在显示顺序里的位置，没给时是末尾；给了却不在时为 None。
fn end_of(order: &[MessageId], id: Option<&MessageId>) -> Option<usize> {
    match id {
        Some(id) => order.iter().position(|known| known == id),
        None => Some(order.len()),
    }
}

/// 回合的开头与结尾（[`HistorySpan::ranges`]）：记录的开头已知；记录之前的区域按 `unrecorded` 往前找。
struct Turns<F> {
    starts: Vec<usize>,
    len: usize,
    unrecorded: F,
}

impl<E, F: FnMut(usize) -> Result<bool, E>> Turns<F> {
    /// 第 `at` 条所在回合的开头：不晚于它的最后一个记录的开头；在第一个记录之前时，不晚于它的最后一条开回合的输入，没有就是它自己。
    fn opening(&mut self, at: usize) -> Result<usize, E> {
        match self.starts.partition_point(|&start| start <= at) {
            0 => {
                for candidate in (0..=at).rev() {
                    if (self.unrecorded)(candidate)? {
                        return Ok(candidate);
                    }
                }
                Ok(at)
            }
            after => Ok(self.starts[after - 1]),
        }
    }

    /// 第 `at` 条所在回合的结尾（不含）：下一个记录的开头，或末尾；第一个记录之前的只到它自己（计划的段只要那条消息的行与它的回合头）。
    fn closing(&self, at: usize) -> usize {
        match self.starts.partition_point(|&start| start <= at) {
            0 => at + 1,
            after => self.starts.get(after).copied().unwrap_or(self.len),
        }
    }
}

/// 历史的一段（[`StoreQueries::range`]，node-store.md §5.6）：`loaded` 只带这一段的消息（模型侧的 `messages` 与只供显示的 `earlier` 各按
/// 自己的顺序）与这一段的事实（回合记录、输入记录、来源、输出头部、工具起止）；会话级的（模型、模式、当前轮、checkpoint、计划）同 `load`。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRange {
    pub loaded: LoadedSession,
    /// 这一段里主线程的 Agent 调用关联到的子会话（同 [`StoreQueries::subagents`] 的取法，只取这一段的）。
    pub subagents: Vec<crate::agents::StoredSubagent>,
    /// `Before` 的段首之前还有存储消息（显示顺序里）。`Calling` 恒为 false。
    pub earlier: bool,
    /// 段首那条存储消息（显示顺序里第一段的第一条）：往前再读一段时作 `Before` 的 `floor`。段是空的时为 None。
    pub floor: Option<MessageId>,
}

/// 冷加载结果：活动分支上的消息（已应用切点与压缩边界）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedSession {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    /// 权限模式（修复线 F20 起不会是 `Plan`；旧实现给出 `Plan` 时按“plan 开、default”读）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    /// plan 开关（修复线 F20；serde 缺省 false）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    /// 还没有被代码回退撤销的文件 checkpoint，按写入顺序。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    /// 库里的当前轮（最后一条真实用户输入）：会话 actor 打开会话时以它为起点（[`FactBatch::turn`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnAnchor>,
    /// 当前轮已经结束、结果是错误（API 错误、轮数用尽等，Node `turn_usage.status = error`）。续写时据此判断最后一条
    /// 没有回答的 user 消息要不要补一条 assistant（docs/specs/rust-cli/session-lifecycle.md §6.3.1）。serde 缺省 false。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub last_turn_failed: bool,
    /// 只供显示的、最后一个压缩点之前的消息（工作线 F102，docs/specs/rust-cli/compact.md §5.1）：应用回退之后的完整链里
    /// 排在 `messages` 之前、不在 `messages` 里的那些，按存储顺序；不含最后一个压缩点的保留段（它们在 `messages` 里）。
    /// 模型侧的历史只是 `messages`；界面显示 `earlier ++ messages`。没有压缩点时为空（serde 缺省为空、为空时不写出）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier: Vec<Message>,
    /// 回合记录（[`TurnRecord`]），按写入顺序（docs/specs/rust-cli/session-lifecycle.md §10.1）。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<TurnRecord>,
    /// 输入记录（[`crate::input_extras::InputRecord`]），按写入顺序（session-lifecycle.md §10.14）。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<crate::input_extras::InputRecord>,
    /// 效果记录（[`InputSource`]，session-lifecycle.md §10.3、§10.31）：这个会话全部消息记录（含回退掉的分支）上的命令 id 与它的消息，
    /// 按写入顺序。输入的消息一定在里面；带命令的批次里别的 user 一侧消息（本地命令的记录等）也在。会话 actor 打开时把这些命令放进
    /// 命令账本（重发执行过的命令回 `Duplicate`，§10.33：以前另有一份去重后的 id 集合）；前端按输入的消息查它，认出这条消息是哪条
    /// 命令的。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<InputSource>,
    /// 工具结果的输出头部（[`crate::command_output::ToolOutputHead`]，session-lifecycle.md §10.34），按存储顺序：库里全部 tool part 带的那些。
    /// 会话 actor 打开时放进视图；前端按调用 id 查。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_heads: Vec<crate::command_output::ToolOutputHead>,
    /// 工具调用的开工与结束（[`crate::metering::ToolTime`]，session-lifecycle.md §10.37），按存储顺序：库里全部 completed / error 的 tool part 的
    /// `state.time`。会话 actor 打开时放进视图；前端按调用 id 查。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_times: Vec<crate::metering::ToolTime>,
    /// ExitPlanMode 拿给用户批准的计划（[`crate::plan::ShownPlan`]，session-lifecycle.md §10.43），按写入顺序：库里的 `rust-cli/plan` entry。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plans: Vec<crate::plan::ShownPlan>,
    /// 被文件回退撤销过的 checkpoint（session-lifecycle.md §10.54）：不在 `checkpoints` 里（不能再回退），它们的回合的文件改动是「已撤销」，回合的
    /// diff 照样从它们算（同 v4）。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reverted: Vec<WorkspaceCheckpoint>,
}

/// 一条输入的消息与提交它的命令（session-lifecycle.md §10.31）：效果记录（§10.3）按消息的那一面。活会话由
/// [`crate::session_api::KernelEvent::InputCommitted`] 给出，冷读由 [`LoadedSession::sources`] 给出，两边同值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSource {
    pub message: MessageId,
    pub command: CommandId,
}

/// `/fork` 的新会话（docs/specs/rust-cli/session-lifecycle.md §6.5.2，Node 分叉对话的形状）：存储在一个事务里写完会话行、
/// 模型与模式、复制的消息、目标状态、时间线与分叉提示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFork {
    pub parent: SessionId,
    pub child: SessionId,
    pub created_at_ms: u64,
    /// 复制到这条消息为止（含）：按用户输入分叉（F75）是这一轮最后一条 assistant。
    pub through: MessageId,
    pub selection: ModelSelection,
    pub mode: PermissionMode,
    /// plan 开关（修复线 F20；serde 缺省 false）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
}

/// [`SessionStore::fork`] 的回执。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForkReceipt {
    /// 新会话写好了：复制的消息数（Node `copiedMessageCount`）。
    Created { copied: usize },
    /// `child` 已经是从 `parent` 分出的会话（调用方重发同一次分叉，docs/specs/rust-cli/session-lifecycle.md §10.57）：什么都没写。
    Existing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub title: String,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFilter {
    pub workspace: WorkspaceRef,
    pub limit: usize,
}

/// 会话列表的排序（修复线 F29，`/resume` 页的 Sort，docs/specs/rust-cli/tui-manage.md §10）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrder {
    /// 按最近活动（`time_updated`）从新到旧。
    #[default]
    Updated,
    /// 按创建时间从新到旧。
    Created,
}

/// ← 项目页与 `/resume` 页的会话查询（修复线 F29，docs/specs/rust-cli/node-store.md §5.1）：App 任务列表的口径，
/// 可以不限目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionQuery {
    pub environment: crate::ids::EnvironmentId,
    /// 只列这个目录的会话；None 是所有目录（“所有项目”）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<crate::env::EnvPath>,
    #[serde(default)]
    pub order: SessionOrder,
    pub limit: usize,
}

/// 查询结果里的一个会话。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    /// 会话名（会话行的 `title`；没有时为空）。
    pub title: String,
    /// 会话的工作目录（会话行的 `directory`）。
    pub root: crate::env::EnvPath,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    /// 首条真实用户输入的文字（界面原文，最多 500 个字符）；还没有输入时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
}

/// [`SessionStore::query_sessions`] 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPage {
    pub sessions: Vec<SessionRecord>,
    /// 超出 `limit` 还有更多会话。
    #[serde(default)]
    pub more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoreError {
    #[error("store access denied: {operation}")]
    Forbidden { operation: String },
    #[error("command conflicts with an existing input: {command_id}")]
    Conflict { command_id: String },
    #[error("store temporarily unavailable: {message}")]
    Unavailable { message: String },
    #[error("store commit outcome unknown; reopen and inspect committed state")]
    OutcomeUnknown,
    #[error("session not found: {session}")]
    NotFound { session: SessionId },
    #[error("storage error: {message}")]
    Storage { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
    /// 要建的会话的 id 已经是另一个会话（fork 带调用方给的 id 时，session-lifecycle.md §10.57）：什么都没写。
    #[error("session already exists: {session}")]
    Exists { session: SessionId },
}

/// 存储的错误到内核 API（工作线 F130，审查 K8）：会话不存在与不支持原样保留，其余给出存储的原文。以前 runtime 与 agents
/// 各写一份这个换算。
impl From<StoreError> for crate::session_api::KernelError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound { session } => Self::NotFound { session },
            StoreError::Unsupported { what } => Self::Unsupported { what },
            StoreError::Exists { session } => Self::Exists { session },
            StoreError::Forbidden { .. }
            | StoreError::Conflict { .. }
            | StoreError::Unavailable { .. }
            | StoreError::OutcomeUnknown
            | StoreError::Storage { .. } => Self::Failed {
                message: error.to_string(),
            },
        }
    }
}

/// 前端记录（缺口 G2，docs/specs/rust-cli/app-protocol.md §5.2）：前端给出的不透明记录，挂在一个会话上。内核与存储
/// 不解释 `kind` 与 `data`：node-store 原样写成 `session_entry`（`type = kind`、`id = key`，同 key 覆盖）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontendRecord {
    pub kind: String,
    pub key: String,
    pub data: Value,
}

/// 会话的生命周期（审查 K7）：建、提交、冷加载、列出、读回会话自己写的 entry、分叉、查询、改名、删除。方法都是必需的：
/// 包装层与新的适配器漏一个就编译失败（以前 16 个方法有缺省的 `Unsupported`）。
///
/// 这个端口不回答“我是哪一种库”：没有 `is_shared` 之类的方法。行为随部署形态变的地方由组装根算成
/// [`crate::session_policy::SessionPolicy`]，输入账本是另一个端口（[`crate::input_journal::InputJournal`]）。
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 建会话并写完初始事实（一个事务）；
    /// `batch` 没有事实也没有命令时只建会话、不提交
    /// （回执序号为 0）：空批次也会推进提交序号，与只建会话的结果不同。
    async fn create(
        &self,
        meta: SessionMeta,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError>;

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError>;

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError>;

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError>;

    /// 这个会话经 `Fact::Entry` 写的某一种记录（[`entry_kinds`]），按写入顺序：`(id, data)`（跨会话的入站与回执在重开
    /// 会话时恢复，docs/specs/rust-cli/cross-session.md §2.2，F28）。只读。
    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, Value)>, StoreError>;

    /// Node `/fork` 的新会话，一个事务写完（[`SessionFork`]）。目标消息不在原会话的 fork 源分支里时整批失败，不留会话行。`child` 已经是从 `parent`
    /// 分出的 fork → [`ForkReceipt::Existing`]；是别的会话 → [`StoreError::Exists`]（都不写，不改写已有的会话）。
    async fn fork(&self, fork: SessionFork) -> Result<ForkReceipt, StoreError>;

    /// 侧聊的新会话，一个事务写完（[`SideChatFork`]，session-lifecycle.md §10.58）：复制父会话到 `base` 为止的历史（只给模型看）与侧聊边界。
    /// `child` 已经是从 `parent` 分出的侧聊 → [`ForkReceipt::Existing`]；是别的会话 → [`StoreError::Exists`]（都不写）。
    async fn side_chat(&self, fork: SideChatFork) -> Result<ForkReceipt, StoreError>;

    /// ← 项目页与 `/resume` 页的会话（修复线 F29，node-store.md §5.1）：只读。
    async fn query_sessions(&self, query: SessionQuery) -> Result<SessionPage, StoreError>;

    /// 给没有打开的会话改名（Node `setCustomSessionTitle`：标题来源记为 custom，不改活动时间）。会话不存在或已删除时
    /// `NotFound`。打开着的会话经它的 actor 改名（`Command::Rename`），不走这里。
    async fn rename(&self, session: &SessionId, title: &str) -> Result<(), StoreError>;

    /// 删除会话（Node `softDeleteSession`：记删除时刻，消息保留，之后不再读出与列出）。会话不存在时 `NotFound`；
    /// 已删除时什么都不做。
    async fn delete(&self, session: &SessionId) -> Result<(), StoreError>;
}

/// 本机设置（审查 K7；Node 的 `local_setting`，不进仓库的设置，与 `RuleSource::LocalSettings` 同一层）：项目的权限规则集与
/// 持久模式、工作区信任，另有群协作会话自己的规则集（在会话行里）。启动、`/permissions` 与信任框读写，不属于某个会话的事务。
#[async_trait]
pub trait LocalSettings: Send + Sync {
    /// 本机项目规则集（Node `local_setting(permission/ruleset)`，shell-permissions.md §5）：启动时读回，作为 LocalSettings
    /// 来源。
    async fn project_rules(
        &self,
        workspace: &WorkspaceRef,
    ) -> Result<crate::permission::ProjectRules, StoreError>;

    /// 改本机项目规则集（`/permissions`，docs/specs/rust-cli/tui-manage.md §1.1）：在一个事务里读当前值、按 Node 的写法
    /// 加或删、upsert，返回落库后的规则集。
    async fn change_project_rules(
        &self,
        workspace: &WorkspaceRef,
        change: ProjectRulesChange,
    ) -> Result<crate::permission::ProjectRules, StoreError>;

    /// 记住这些附加工作目录（没有会话时的 `/add-dir` 的「记住」，docs/specs/rust-cli/permission-updates.md §6.1）：在一个事务里读
    /// 本机项目规则集、`additionalDirectories` 追加去重、upsert（同 [`Fact::DirectoriesRemembered`]，有会话时走那一条）。
    async fn remember_directories(
        &self,
        workspace: &WorkspaceRef,
        directories: Vec<crate::env::EnvPath>,
    ) -> Result<(), StoreError>;

    /// 会话自己的规则集（Node `loadProjectPermissionRuleset`，修复线 F2）：Node 群协作会话的
    /// `session.permission.scope` 是 `session`，规则与模式只在会话行里，不读也不写本机项目的规则与持久模式。
    /// 其他会话为 None（规则在 [`Self::project_rules`]）。
    async fn session_rules(
        &self,
        session: &SessionId,
    ) -> Result<Option<crate::permission::ProjectRules>, StoreError>;

    /// 本机项目的持久模式（Node `getProjectPermissionMode`，docs/specs/rust-cli/config.md §2.2）：启动时读一次，作为新会话
    /// 的缺省模式；没有或不合法时为 None。
    async fn project_mode(
        &self,
        workspace: &WorkspaceRef,
    ) -> Result<Option<crate::permission::PermissionMode>, StoreError>;

    /// 工作区信任（docs/specs/rust-cli/workspace-trust.md §2.3）：按给出的顺序，返回 `candidates` 里第一个有信任记录的键；
    /// 都没有时为 None。只读。
    async fn trusted_workspace(
        &self,
        candidates: &[crate::workspace_trust::TrustKey],
    ) -> Result<Option<crate::workspace_trust::TrustKey>, StoreError>;

    /// 记下一个工作区已信任（upsert，workspace-trust.md §2.3）。
    async fn trust_workspace(
        &self,
        key: &crate::workspace_trust::TrustKey,
    ) -> Result<(), StoreError>;
}

/// 用量的只读查询与前端记录（审查 K7）：前端经 `Kernel::queries` 直接用，不经会话 actor（这些都不是会话状态）。以前
/// runtime 为它们各写一个透传的 `Kernel` 方法，加一个查询要改 9 个文件。
#[async_trait]
pub trait StoreQueries: Send + Sync {
    /// `/usage` 的 API 时长（docs/specs/rust-cli/usage.md §4）：会话及其子代理子会话里成功请求的耗时之和。
    async fn metering(
        &self,
        session: &SessionId,
    ) -> Result<crate::metering::SessionMetering, StoreError>;

    /// footer 的请求统计（docs/specs/rust-cli/usage.md §4.1，修复线 F27）：本会话完成的主回合请求在用量表里的聚合；
    /// 修复线 F112 起子会话给出它完成的子代理请求（子代理的行只在子会话，主会话的结果不变）。每次都从行聚合，不缓存。
    async fn request_stats(
        &self,
        session: &SessionId,
    ) -> Result<crate::metering::RequestStats, StoreError>;

    /// 活动统计的原始聚合（usage.md §5.2）：只读的聚合查询。今天与时区偏移由调用方给（`Kernel::activity` 取注入的时钟）。
    async fn activity(
        &self,
        query: crate::metering::ActivityQuery,
    ) -> Result<crate::metering::ActivityStats, StoreError>;

    /// 写一条前端记录（缺口 G2）：同 key 覆盖，不推进会话的活动时间。会话不存在时 `NotFound`。
    async fn put_record(
        &self,
        session: &SessionId,
        record: &FrontendRecord,
    ) -> Result<(), StoreError>;

    /// 这个会话里某一种前端记录，按写入顺序（缺口 G2）。
    async fn records(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<FrontendRecord>, StoreError>;

    /// 这些会话的持久事实（docs/specs/rust-cli/session-facts.md §2），按给出的顺序；库里没有的不给出，删除了的照样给出
    /// （`deleted_at_ms` 有值）。不限工作区与种类：调用方按 `workspace` 隔离。
    async fn session_facts(
        &self,
        sessions: &[SessionId],
    ) -> Result<Vec<crate::session_facts::SessionFacts>, StoreError>;

    /// 这个会话在库里的分支切点（docs/specs/rust-cli/session-facts.md §2）：会话行的回退游标原文，没有切点为 None；会话不在为 `NotFound`
    /// （删除了的照样给）。只用来比较「库里的分支变没变」：回退游标不在会话事实里，只写它的提交不推进活动时间。
    async fn branch_cut(&self, session: &SessionId) -> Result<Option<String>, StoreError>;

    /// 这个工作区里最近活动的列表会话（含软删除的）的事实，按活动时间从新到旧（同一时刻 id 大的在前），至多 `limit` 个。
    async fn recent_session_facts(
        &self,
        workspace: &WorkspaceRef,
        limit: usize,
    ) -> Result<Vec<crate::session_facts::SessionFacts>, StoreError>;

    /// 这个会话派生过的子代理（session-lifecycle.md §10.46）：活动分支上主线程的 Agent 调用关联到的子会话，按第一次出现的先后。只读。
    async fn subagents(
        &self,
        _session: &SessionId,
    ) -> Result<Vec<crate::agents::StoredSubagent>, StoreError> {
        Err(StoreError::Unsupported {
            what: "subagents".to_owned(),
        })
    }

    /// 会话历史里的一段（docs/specs/rust-cli/node-store.md §5.5）：`records` 里每条存储的消息（[`Message::record`] 的 id）按冷读取的换法换成
    /// 内核消息，按给出的顺序；工具结果随它的 assistant、压缩边界随它的摘要。带上这些消息里工具调用的输出头部与起止、开头在这一段里的回合记录。
    /// 库里没有的 id 不给出。只读，不选分支（调用方给的就是它上一次整份冷读里的消息）。前端分页时只重建一页的行用它。
    async fn history(
        &self,
        _session: &SessionId,
        _records: &[MessageId],
    ) -> Result<HistorySlice, StoreError> {
        Err(StoreError::Unsupported {
            what: "history slices".to_owned(),
        })
    }

    /// 历史的一段（docs/specs/rust-cli/node-store.md §5.6）：只读消息索引、压缩摘要与保留段、段里的消息与 part。前端的冷读只读最后
    /// 一段、往前翻页与目录按段读用它；TUI 与 `-p` 照旧 [`SessionStore::load`] 整段。会话不在或已删除时 `NotFound`；`Before` 的 `floor` 不在活动分支上时
    /// 也是 `NotFound`（历史已经变了）。
    async fn range(
        &self,
        _session: &SessionId,
        _span: HistorySpan,
    ) -> Result<HistoryRange, StoreError> {
        Err(StoreError::Unsupported {
            what: "history ranges".to_owned(),
        })
    }

    /// 列表会话的事实变了（session-facts.md §3）：每个改了它的写事务提交之后一项（提交后的值），按提交的顺序；订阅之前的
    /// 不补（先订阅、再读 [`Self::recent_session_facts`]）。这是会话列表增量的唯一来源。
    fn session_fact_changes(&self) -> crate::session_facts::FactStream;
}

/// 一个存储适配器实现的全部会话端口（组装根与 runtime 的 `Ports` 用；各能力只拿自己用的那一个）。node-store 与测试的
/// `MemoryStore` 都实现三个；以后的云端适配器也要实现全部，不会悄悄缺功能。
pub trait Store:
    SessionStore + LocalSettings + StoreQueries + crate::deletion::SessionPurge
{
}

impl<T: SessionStore + LocalSettings + StoreQueries + crate::deletion::SessionPurge + ?Sized> Store
    for T
{
}

/// Artifact 的用途：决定是否进入文件选择器、保留策略（cloud 规则 7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactPurpose {
    ToolOutput,
    Attachment,
    Media,
    Checkpoint,
    Deliverable,
}

/// Artifact 引用（Node `zcode-artifact://<sessionId>/<artifactId>`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub session: SessionId,
    pub id: ArtifactId,
    pub purpose: ArtifactPurpose,
    pub media_type: String,
    pub size: u64,
}

#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// 先写对象并核验，再返回引用；调用方拿到引用后才能把它提交进会话事实。
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError>;

    async fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError>;

    /// 按会话与 artifact id 找回完整引用（修复线 F18，docs/specs/rust-cli/tools-files.md §1.4）。模型交回的落盘
    /// 位置 `zcode-artifact://<会话>/<artifact>` 只有这两项；大小、用途与类型由实现方从自己的记录取，内核不凭空
    /// 造引用。读内容仍经 [`Self::get`]（照常核对大小）。找不到时为 `None`。不支持的实现返回 `Unsupported`。
    async fn find(
        &self,
        _session: &SessionId,
        _id: &ArtifactId,
    ) -> Result<Option<ArtifactRef>, StoreError> {
        Err(StoreError::Unsupported {
            what: "artifact lookup by id".to_owned(),
        })
    }
}

/// `Fact::Entry` 的种类（不带 `rust-cli/` 前缀；node-store 写成 `type = rust-cli/<种类>`、键 `<会话>:rust-cli/<种类>:<id>`）。
/// 写的一方（session、agents）与读回的一方（node-store、`SessionStore::entries`）共用这里的名字（F128，审查 X10：以前在
/// session 与 node-store 各写一份）。
///
/// 工作线 F130 删掉 `pending_interaction` 与 `interaction_resolved`：待回答的交互只在会话 actor 的内存里，不再写（库里的旧
/// 记录不读）。
pub mod entry_kinds {
    /// 跨会话收到的消息（docs/specs/rust-cli/cross-session.md §2.2）。
    pub const PEER_MESSAGE: &str = "peer_message";
    /// 跨会话的消息已处理。
    pub const PEER_RESOLVED: &str = "peer_resolved";
    /// 跨会话的回执。
    pub const PEER_RECEIPT: &str = "peer_receipt";
    /// 跨会话的消息或回执已并入对话：与并入的那条消息同一批提交（cross-session.md §2.2，F139）。
    pub const PEER_MERGED: &str = "peer_merged";
    /// 分叉的子代理会话记下父会话（agents）。
    pub const FORK_PARENT: &str = "fork_parent";
    /// 回合记录（[`super::TurnRecord`]，session-lifecycle.md §10.1）。
    pub const TURN: &str = "turn";
    /// 输入记录（[`crate::input_extras::InputRecord`]，session-lifecycle.md §10.14）：键是用户消息的 id。
    pub const INPUT: &str = "input";
    /// ExitPlanMode 拿给用户批准的计划（[`crate::plan::ShownPlan`]，session-lifecycle.md §10.43）：键是调用 id。
    pub const PLAN: &str = "plan";
    /// 会话钉住的 system 输入（[`crate::session_prompt::SystemPin`]，context.md §15.3）：键固定，后写覆盖。
    pub const SYSTEM_PIN: &str = "system_pin";
}

// 回合记录与它的落库写法放在分出的文件里（800 行上限，PROJ-GAPS-2 加开回合的后台工作时拆出；路径与名字不变）。
#[path = "store_turn.rs"]
mod turn;
pub use turn::{TurnEnded, TurnRecord};

// 侧聊的新会话（session-lifecycle.md §10.58）：类型与常量放在分出的文件里（800 行上限）。
#[path = "store_side_chat.rs"]
pub mod side_chat;
pub use side_chat::{SideChatBase, SideChatFork};

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
