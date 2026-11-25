//! 输入的注记与执行覆盖的退回事实（docs/specs/rust-cli/session-lifecycle.md §10.48）：注记是前端给这条输入的 JSON 对象，在输入记录里，
//! 前端经 `Command::PatchNote` 补字段；退回是主循环的事实，会话 actor 记进这一轮的回合记录。两样都先改记录、再发事件。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{MessageId, TurnId};
use rust_cli_kernel_api::session_api::{Ack, KernelEvent};
use rust_cli_kernel_api::turn_execution::FellBack;
use serde_json::{Map, Value};

use crate::actor::{Actor, accepted, rejected};

impl Actor {
    /// `Command::PatchNote`：消息要在会话的历史里，它的输入记录要带注记；合并之后同一条输入记录再写一次，成功了才换掉内存里的、发 `InputRecorded`。
    pub(crate) async fn patch_note(
        &mut self,
        message: &MessageId,
        patch: &Map<String, Value>,
    ) -> Ack {
        if !self.messages.iter().any(|kept| kept.id() == message) {
            return rejected(Refusal::NotFound, "the input is not in the conversation");
        }
        let Some((at, record)) = self
            .inputs
            .iter()
            .enumerate()
            .find(|(_, record)| record.message == *message)
            .and_then(|(at, record)| Some((at, record.patched(patch)?)))
        else {
            return rejected(Refusal::Unavailable, "the input has no note");
        };
        match self.commit(None, vec![record.fact()]).await {
            Ok(()) => {
                self.inputs[at] = record.clone();
                self.log.append(KernelEvent::InputRecorded { record });
                accepted()
            }
            Err(error) => rejected(Refusal::Failed, error.to_string()),
        }
    }

    /// 主循环退回了（`QueryEvent::ExecutionFellBack`）：记进这一轮的回合记录（只在内存，回合结束时与结局同一次写），返回要发的事件。
    /// 回合记录在这一轮第一条消息落库时就有了，退回在输入落库之后，所以总找得到。
    pub(crate) fn fell_back(&mut self, turn: TurnId, reason: String) -> KernelEvent {
        let fell_back = FellBack {
            reason,
            at: self.deps.clock.now_ms(),
        };
        if let Some(record) = self
            .turns
            .iter_mut()
            .rev()
            .find(|record| record.turn == turn)
        {
            record.fell_back = Some(fell_back.clone());
        }
        KernelEvent::ExecutionFellBack { turn, fell_back }
    }
}

#[cfg(test)]
#[path = "note_tests.rs"]
mod tests;
