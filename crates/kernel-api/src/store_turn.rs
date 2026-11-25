//! 回合记录（[`TurnRecord`]，session-lifecycle.md §10.1）与它的落库写法（`rust-cli/turn` entry）：从 store.rs 分出（800 行上限）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Fact, entry_kinds};
use crate::ids::{CommandId, MessageId, TurnId};

/// 一个回合的记录（session-lifecycle.md §10.1，v5 的 K-I1）：会话 actor 给每个回合（人类输入、后台通知、定时任务、
/// 压缩……）落一条，与这个回合第一条落库的消息同一批提交。库里的回合只按真实用户输入划分（`anchor.turnId`，usage.md
/// D7），这条记录让冷读也能把每条消息还原到活会话里的那个回合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub turn: TurnId,
    /// 这个回合第一条落库的消息：按显示顺序走消息，遇到它就换到这个回合。
    pub first: MessageId,
    /// 回合的来源与开始时刻（session-lifecycle.md §10.8），与 `TurnStarted` 同值。
    pub origin: crate::session_api::TurnOrigin,
    pub started_at: u64,
    /// 开这个回合的命令（`TurnStarted.command`，session-lifecycle.md §10.31）：人类输入、bash 模式与压缩的回合有，通知、定时任务、
    /// 同伴消息开的回合没有。较早写的记录没有，serde 缺省 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandId>,
    /// 回合怎样结束（session-lifecycle.md §10.17）：回合结束时同一条记录再写一次；还在跑、或进程在它结束之前退出的为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<TurnEnded>,
    /// 这一轮的执行覆盖退回了（session-lifecycle.md §10.48）：退回时只改内存里的记录，回合结束时与结局同一次写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fell_back: Option<crate::turn_execution::FellBack>,
    /// 后台结果开的回合：开它的那几项后台工作（`TurnStarted.works`，session-lifecycle.md §10.50）。较早写的记录与别的回合没有，读成空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub works: Vec<crate::background::NoticedWork>,
    /// 这一轮主循环里的压缩（自动、反应式，session-lifecycle.md §10.52），按开始的先后：开始与结局只改内存里的记录，回合结束时与结局同一次写。
    /// 较早写的记录与没有这类压缩的回合没有它，读成空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compactions: Vec<crate::compaction::LoopCompaction>,
}

/// 一个回合的结局（§10.17）：冷读的回合头与工具行按它给出真实的结局。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEnded {
    /// 与这个回合里输入的结局同一个闭集（成功含 hook 要求停下、被中断、失败）。
    pub outcome: crate::session_api::InputEnd,
    /// 因停止而取消的工具调用（结果是拒绝的文字，冷读不把它们当成出错）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled: Vec<crate::ids::ToolUseId>,
    /// 回合结束的时刻与用时（与 `TurnFinished.time` 同值，session-lifecycle.md §10.36）。较早写的记录没有，读成 None、不补。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<crate::session_api::TurnTime>,
    /// 这个回合的文件改动（与 `TurnFinished.files` 同值，session-lifecycle.md §10.42）。较早写的记录没有，读成空、不补。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::session_api::TurnFileChange>,
}

/// 回合记录的 `data` 里第一条消息的键。
const TURN_FIRST_KEY: &str = "first";
/// 回合记录的 `data` 里来源的键。
const TURN_ORIGIN_KEY: &str = "origin";
/// 回合记录的 `data` 里开始时刻的键。
const TURN_STARTED_AT_KEY: &str = "startedAt";
/// 回合记录的 `data` 里结局的键（回合结束时再写的同一条记录，§10.17）。
const TURN_ENDED_KEY: &str = "ended";
/// 回合记录的 `data` 里开回合的命令的键（§10.30）。
const TURN_COMMAND_KEY: &str = "command";
/// 回合记录的 `data` 里执行覆盖退回的键（§10.48）。
const TURN_FELL_BACK_KEY: &str = "fellBack";
/// 回合记录的 `data` 里开回合的后台工作的键（§10.50）。
const TURN_WORKS_KEY: &str = "works";
/// 回合记录的 `data` 里主循环的压缩的键（§10.52）。
const TURN_COMPACTIONS_KEY: &str = "compactions";

impl TurnRecord {
    /// 刚开始的回合的记录：还没有结局，没有命令、退回、后台工作与主循环的压缩（测试与各处按需再改字段，加字段时不用改调用方）。
    pub fn opened(
        turn: TurnId,
        first: MessageId,
        origin: crate::session_api::TurnOrigin,
        started_at: u64,
    ) -> Self {
        Self {
            turn,
            first,
            origin,
            started_at,
            command: None,
            ended: None,
            fell_back: None,
            works: Vec::new(),
            compactions: Vec::new(),
        }
    }

    /// 这个回合的开始（与 `TurnStarted` 同值，session-lifecycle.md §10.33）。
    pub fn start(&self) -> crate::session_api::TurnStart {
        crate::session_api::TurnStart {
            turn: self.turn.clone(),
            command: self.command.clone(),
            origin: self.origin,
            started_at: self.started_at,
            works: self.works.clone(),
        }
    }

    /// 落库的写法（`Fact::Entry`，种类 [`entry_kinds::TURN`]，键是回合 id）。
    pub fn fact(&self) -> Fact {
        let mut data = serde_json::json!({
            TURN_FIRST_KEY: self.first,
            TURN_ORIGIN_KEY: self.origin,
            TURN_STARTED_AT_KEY: self.started_at,
        });
        if let Some(command) = &self.command {
            data[TURN_COMMAND_KEY] = serde_json::json!(command);
        }
        if let Some(ended) = &self.ended {
            data[TURN_ENDED_KEY] = serde_json::json!(ended);
        }
        if let Some(fell_back) = &self.fell_back {
            data[TURN_FELL_BACK_KEY] = serde_json::json!(fell_back);
        }
        if !self.works.is_empty() {
            data[TURN_WORKS_KEY] = serde_json::json!(self.works);
        }
        if !self.compactions.is_empty() {
            data[TURN_COMPACTIONS_KEY] = serde_json::json!(self.compactions);
        }
        Fact::Entry {
            kind: entry_kinds::TURN.to_owned(),
            id: self.turn.as_str().to_owned(),
            data,
        }
    }

    /// 读回 [`Self::fact`] 写的 entry（`SessionStore::entries` 的一项）；形状不对时为 None。
    pub fn from_entry(id: &str, data: &Value) -> Option<Self> {
        Some(Self {
            turn: TurnId::new(id),
            first: MessageId::new(data[TURN_FIRST_KEY].as_str()?),
            origin: serde_json::from_value(data[TURN_ORIGIN_KEY].clone()).ok()?,
            started_at: data[TURN_STARTED_AT_KEY].as_u64()?,
            // 较早写的记录没有开回合的命令（§10.30）
            command: data
                .get(TURN_COMMAND_KEY)
                .and_then(Value::as_str)
                .map(CommandId::new),
            // 较早写的记录（与还没结束的回合）没有结局
            ended: data
                .get(TURN_ENDED_KEY)
                .and_then(|ended| serde_json::from_value(ended.clone()).ok()),
            // 较早写的记录与没有退回的回合没有它
            fell_back: data
                .get(TURN_FELL_BACK_KEY)
                .and_then(|fell_back| serde_json::from_value(fell_back.clone()).ok()),
            // 较早写的记录与不是后台结果开的回合没有它
            works: data
                .get(TURN_WORKS_KEY)
                .and_then(|works| serde_json::from_value(works.clone()).ok())
                .unwrap_or_default(),
            // 较早写的记录与主循环里没有压缩的回合没有它
            compactions: data
                .get(TURN_COMPACTIONS_KEY)
                .and_then(|compactions| serde_json::from_value(compactions.clone()).ok())
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
#[path = "store_turn_tests.rs"]
mod tests;
