//! 用户输入行（`userInput`）的写法（protocol-v5.md §15.3.3、§24.1、§46）：输入记录给的附件与引用、产生它的命令、这一轮的高速卡。
//! 活会话与由视图建走同一组函数。高速卡是输入记录的注记拼上这一轮的退回事实（§46.1）：注记来自命令与上报，退回来自回合，各一个来源。

use std::collections::BTreeMap;

use rust_cli_kernel_api::ids::{MessageId, TurnId};
use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::turn_execution::FellBack;
use serde_json::{Value, json};

use super::edit::Edit;
use super::{Projection, inputs};
use crate::publisher::Fields;
use crate::session::rows::{self, ROWS, keys};

/// 注记里的卡号（契约 `HighspeedTurnMeta.cardId`）与归因的键（`ErrorAttribution.highspeedCardId`）。
const CARD_ID: &str = "cardId";
const ATTRIBUTION_CARD: &str = "highspeedCardId";

/// 视图里各回合的退回事实（回合 id → 退回）：在跑回合的只在内核的内存里，也在视图里（session-lifecycle.md §10.48）。
pub(super) fn fell_back(view: &SessionView) -> BTreeMap<String, FellBack> {
    view.turns
        .iter()
        .filter_map(|record| Some((record.turn.as_str().to_owned(), record.fell_back.clone()?)))
        .collect()
}

impl Projection {
    /// 一条用户输入的行（冷读与 `MessageDone` 同一个函数）。
    pub(super) fn user_row(
        &mut self,
        edit: &mut Edit<'_>,
        (turn, message): (&str, &Message),
        (id, text, guided, created): (String, String, bool, Option<u64>),
    ) {
        self.segment_input(edit, turn, (&id, guided, created));
        let seq = self.seq_of(edit, &id);
        let mut fields = rows::user_input(&text, guided, turn, seq, created);
        if let Some(record) = self.records.get(message.id()) {
            let content = match message {
                Message::User(user) => user.content.as_slice(),
                _ => &[],
            };
            inputs::decorate(&mut fields, record, content);
        }
        // 修 bug（§23）：以前不写，界面的发送漏斗等不到这条命令的行，每次发送都在 30 s 后记成失败
        rows::set_command(&mut fields, self.sources.get(message.id()));
        self.set_card(&mut fields, message.id(), turn);
        edit.put(ROWS, &id, fields);
    }

    /// 这一行的高速卡：有注记才写。退回事实按行所在的回合取（§46.1）。
    fn set_card(&self, fields: &mut Fields, message: &MessageId, turn: &str) {
        if let Some(note) = self
            .records
            .get(message)
            .and_then(|record| record.note.as_ref())
        {
            rows::set_highspeed(fields, note, self.fell_back.get(turn));
        }
    }

    /// 输入记录（`InputRecorded`）：先于它的消息到时只记下；上报之后再来时这一行已经在，换上合并后的注记（§46.1）。
    pub(super) fn recorded(&mut self, edit: &mut Edit<'_>, record: &InputRecord) {
        self.records.insert(record.message.clone(), record.clone());
        if let Some(mut fields) = edit.get(ROWS, record.message.as_str()).cloned() {
            let turn = rows::text(&fields, keys::TURN_ID).to_owned();
            self.set_card(&mut fields, &record.message, &turn);
            edit.put(ROWS, record.message.as_str(), fields);
        }
    }

    /// 这一轮退回了（`ExecutionFellBack`）：记下；这一轮里带注记的输入行重写。用户行早于它建好（输入先落库），晚于它建时 [`Self::set_card`] 读到它。
    pub(super) fn fell_back(&mut self, edit: &mut Edit<'_>, turn: &TurnId, fell_back: &FellBack) {
        self.fell_back
            .insert(turn.as_str().to_owned(), fell_back.clone());
        let noted: Vec<MessageId> = self
            .records
            .values()
            .filter(|record| record.note.is_some())
            .map(|record| record.message.clone())
            .collect();
        for message in noted {
            if let Some(mut fields) = edit
                .get(ROWS, message.as_str())
                .filter(|fields| rows::text(fields, keys::TURN_ID) == turn.as_str())
                .cloned()
            {
                self.set_card(&mut fields, &message, turn.as_str());
                edit.put(ROWS, message.as_str(), fields);
            }
        }
    }

    /// 回合失败的归因（`lastError.attribution`，§46.1）：这一轮带注记的输入行的卡号；没有这样的行是 null。
    pub(super) fn attribution(&self, edit: &Edit<'_>, turn: &str) -> Value {
        let card = self.records.values().find_map(|record| {
            let in_turn = edit
                .get(ROWS, record.message.as_str())
                .is_some_and(|fields| rows::text(fields, keys::TURN_ID) == turn);
            in_turn
                .then(|| {
                    record
                        .note
                        .as_ref()?
                        .get(CARD_ID)?
                        .as_str()
                        .map(str::to_owned)
                })
                .flatten()
        });
        json!({ ATTRIBUTION_CARD: card })
    }

    /// 这一行带不带高速卡（`row.reportHighspeed` 的前提，§46.2）：输入行的 id 就是它的消息 id。
    pub(crate) fn noted(&self, id: &str) -> bool {
        self.records
            .get(&MessageId::new(id))
            .is_some_and(|record| record.note.is_some())
    }
}
