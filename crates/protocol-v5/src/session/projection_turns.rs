//! 没有回合记录的历史的回合（protocol-v5.md §75.3）：哪条消息开回合由 kernel-api 的 `TurnWalk` 判（建行的循环在 `Projection::built`），这里给这种回合建回合头。

use rust_cli_kernel_api::ids::{MessageId, TurnId};
use rust_cli_kernel_api::session_api::{TurnOrigin, TurnStart};

use super::Projection;
use super::edit::Edit;
use super::rows::turn_state;

impl Projection {
    /// 没有记录的回合的回合头（§75.3）：来源是用户输入，开始的时刻是这条输入的消息 id 里的毫秒（两边写的消息 id 都带），命令是这条输入的命令；
    /// 库里没有这个回合的结局，同没有结局的回合记录读成完成。
    pub(super) fn unrecorded_header(
        &mut self,
        edit: &mut Edit<'_>,
        turn: TurnId,
        input: &MessageId,
    ) {
        let start = TurnStart {
            turn,
            command: self.sources.get(input).cloned(),
            origin: TurnOrigin::Input,
            started_at: input.created_ms().unwrap_or_default(),
            works: Vec::new(),
        };
        self.header(edit, &start, turn_state::COMPLETED);
    }
}
