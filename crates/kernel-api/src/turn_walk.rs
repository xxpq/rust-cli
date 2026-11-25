//! 按显示顺序把消息归到回合（docs/specs/rust-cli/node-store.md §5.6）：有回合记录的按记录；没有记录的历史（Node 写的、较早的本工程会话）按开回合的
//! 用户输入切，回合 id 由那条输入的消息 id 推出。前端建行与范围读补段首都用这一个判定，所以同一条消息在冷读、激活、往前补、按片重建里是同一个回合。
//!
//! ```text
//! 当前回合 = 无 | 记录的回合 R | 合成的回合 S
//!   是某条记录的 first ─────────────────────────────► R
//!   是开回合的用户输入 ─┬ 当前是 R，输入不晚于 R 的结束 ─► 仍是 R（R 自己的输入、成批进 R 的后续输入；R 没有结束时刻时同样）
//!                        └ 否则 ─────────────────────────► S = UNRECORDED_TURN_PREFIX + 这条消息的 id
//!   其余 ───────────────────────────────────────────► 不变（无 = 会话开头、第一条输入之前）
//! ```

use std::collections::HashMap;

use crate::ids::{MessageId, TurnId};
use crate::message::Message;
use crate::store::TurnRecord;

/// 合成的回合 id 的前缀，后接开它的那条输入的消息 id。不直接用消息 id：前端的行按 id 存在一个集合里，回合头行的 id 是回合 id，用户输入行的 id 是消息 id，两者会撞。
pub const UNRECORDED_TURN_PREFIX: &str = "turn_of_";

/// 一条消息是不是开回合的用户输入（[`crate::user_input::opens_turn`]：Human 来源，不是中断提示、不是 bash 模式的输出）。前端把同一组消息建成用户输入行。
pub fn opens_turn(message: &Message) -> bool {
    matches!(message, Message::User(user) if crate::user_input::opens_turn(user))
}

/// 由这条输入开的、没有记录的回合的 id。
pub fn unrecorded_turn(input: &MessageId) -> TurnId {
    TurnId::new(format!("{UNRECORDED_TURN_PREFIX}{}", input.as_str()))
}

/// 走到一条消息时开的回合。
#[derive(Debug, Clone, PartialEq)]
pub enum Opened<'a> {
    /// 回合记录：这条消息是它的 `first`。
    Recorded(&'a TurnRecord),
    /// 没有记录的回合：这条输入开的。
    Unrecorded(TurnId),
}

/// 按显示顺序走消息的状态。
#[derive(Debug)]
pub struct TurnWalk<'a> {
    /// 回合记录按它的第一条消息（内核消息的 id）；同一条消息有两条记录时取后写的（同前端建行以前的读法）。
    starts: HashMap<&'a MessageId, &'a TurnRecord>,
    /// 当前回合是记录的回合时是它。
    recorded: Option<&'a TurnRecord>,
}

impl<'a> TurnWalk<'a> {
    pub fn new(records: &'a [TurnRecord]) -> Self {
        Self {
            starts: records
                .iter()
                .map(|record| (&record.first, record))
                .collect(),
            recorded: None,
        }
    }

    /// 走到这条消息：它开了哪个回合（不开为 None，仍在当前回合里）。
    pub fn step(&mut self, message: &Message) -> Option<Opened<'a>> {
        if let Some(record) = self.starts.get(message.id()).copied() {
            self.recorded = Some(record);
            return Some(Opened::Recorded(record));
        }
        if !opens_turn(message)
            || self
                .recorded
                .is_some_and(|record| !ended_before(record, message))
        {
            return None;
        }
        self.recorded = None;
        Some(Opened::Unrecorded(unrecorded_turn(message.id())))
    }
}

/// 这条消息建于回合记录的结束之后（两样时刻都有时才比）：本工程每个回合都有记录，回合结束之后不再往这个回合里写，所以这时开回合的输入是别的写者
/// （Node）续写的。本工程成批进回合的后续输入（query.md §4.3）建于回合里，留在回合里。
fn ended_before(record: &TurnRecord, message: &Message) -> bool {
    let ended = record
        .ended
        .as_ref()
        .and_then(|ended| ended.time)
        .map(|time| time.ended_at);
    matches!((ended, message.id().created_ms()), (Some(ended), Some(created)) if ended < created)
}

#[cfg(test)]
#[path = "turn_walk_tests.rs"]
mod tests;
