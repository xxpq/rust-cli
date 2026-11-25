//! 行 → 存储消息的索引（protocol-v5.md §71.2）：投影走过的每条消息（由视图建与活会话的 `MessageDone` 都经 `message()`）记一项：
//! 它在库里所在的那条消息（[`Message::record`]）、它的回合、它的第一个行号。`rows.range` 碰到骨架行时按它找出这一页要读的那几条存储的消息，
//! 只读这些、只建这些（不再整份冷读）。随投影建、随投影丢。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::Message;

/// 走过的消息，按走的先后（显示顺序）。
#[derive(Debug, Default)]
pub(crate) struct Walk {
    items: Vec<Item>,
    /// 回合 id（相邻的同一回合只存一份）；`Item::turn` 是它的下标。空串 = 不属于回合。
    turns: Vec<String>,
    /// 上一条消息处理完时的下一个行号：两条消息之间建的行（回合头、上一条之后的主循环压缩标记、这一条的流式行）都算这一条的。
    mark: u64,
}

#[derive(Debug)]
struct Item {
    record: MessageId,
    turn: usize,
    /// 这一项的第一个行号（行号单调，所以各项的它不减）。
    first_row: u64,
}

impl Walk {
    /// 第一行的号是 `first_row` 的投影的索引（§73.1：只建了后缀时不从 0 起）。
    pub(super) fn starting_at(first_row: u64) -> Self {
        Self {
            mark: first_row,
            ..Self::default()
        }
    }

    /// 往前补的一段（§73.3）接到前面：那一段的索引已按往前编的号平移过（`shift` 加到它的每一项的行号上），它的项都在这一份之前。
    pub(crate) fn prepend(&mut self, earlier: Walk, shift: i64) {
        let Walk {
            mut items, turns, ..
        } = earlier;
        let offset = turns.len();
        for item in &mut items {
            item.first_row = item.first_row.saturating_add_signed(shift);
        }
        for item in &mut self.items {
            item.turn += offset;
        }
        let mut merged_turns = turns;
        merged_turns.append(&mut self.turns);
        items.append(&mut self.items);
        self.items = items;
        self.turns = merged_turns;
    }

    /// `message()` 开头：走到这条消息。
    pub(super) fn enter(&mut self, message: &Message, turn: &str) {
        if self.turns.last().is_none_or(|last| last != turn) {
            self.turns.push(turn.to_owned());
        }
        self.items.push(Item {
            record: message.record().clone(),
            turn: self.turns.len() - 1,
            first_row: self.mark,
        });
    }

    /// `message()` 结尾：这条消息处理完，之后建的行算下一条的。
    pub(super) fn leave(&mut self, next_row: u64) {
        self.mark = next_row;
    }

    /// 一页里的骨架行要读的存储消息（§71.2），按走的先后、不重复。每一行给出它的回合与定位用的行号：有回合的行用回合头的行号（从那一项起，前后
    /// 同一回合的都要：一个回合的行互相依赖，整回合一起建）；没有回合的行用自己的行号（行号落在哪一项就是哪一条）。
    pub(crate) fn records<'a>(
        &self,
        rows: impl IntoIterator<Item = (u64, &'a str)>,
    ) -> Vec<MessageId> {
        let mut picked = std::collections::BTreeSet::new();
        for (anchor, turn) in rows {
            let Some(at) = self
                .items
                .partition_point(|item| item.first_row <= anchor)
                .checked_sub(1)
            else {
                continue;
            };
            if turn.is_empty() {
                picked.insert(at);
                continue;
            }
            let same = |index: usize| self.turns[self.items[index].turn] == turn;
            let mut start = at;
            while start > 0 && same(start - 1) {
                start -= 1;
            }
            let mut end = at;
            while end + 1 < self.items.len() && same(end + 1) {
                end += 1;
            }
            if same(at) {
                picked.extend(start..=end);
            }
        }
        let mut records: Vec<MessageId> = picked
            .into_iter()
            .map(|index| self.items[index].record.clone())
            .collect();
        records.dedup();
        records
    }
}

#[cfg(test)]
#[path = "projection_walk_tests.rs"]
mod tests;
