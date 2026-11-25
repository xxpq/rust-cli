//! 输出上限之后的续写拼进同一行（protocol-v5.md §19.9，13 Q16；照 v4 的 product projection）：内核在续写之前提交一条 meta 消息
//! （来源 `OUTPUT_LIMIT_RESUME_SOURCE`），它之后的那条 assistant 的正文接进上一条的正文行。活会话与冷读走 `message()` 的同一条路；
//! 流式只是提前接上，`MessageDone` 按最终内容定。

use rust_cli_kernel_api::message::{OUTPUT_LIMIT_RESUME_SOURCE, UserMessage, UserOrigin};

use super::*;

/// 续写要接进的行：它的 id、续写之前的正文与 `assistantResponseId`（放回时用）。
#[derive(Debug, Clone)]
pub(super) struct Continuation {
    row: String,
    base: String,
    response: String,
}

impl Projection {
    /// 一条消息对续写的影响（`message()` 在建行之前调）；返回这条 assistant 的正文要接进的行。
    /// - 续写提示，且最后一行是这个回合的正文行 → 记下它；
    /// - assistant → 取走记下的；显示的第一部分是正文才接（续写先出思考时，v4 的思考行先成了最后一行，也不接）。
    ///   流式时提前接过、最终不接的，放回续写之前的正文；
    /// - 别的消息是边界，清掉。
    pub(super) fn continuation_of(
        &mut self,
        edit: &mut Edit<'_>,
        turn: &str,
        message: &Message,
        first: Option<&Part>,
    ) -> Option<Continuation> {
        match message {
            Message::User(UserMessage {
                origin: UserOrigin::Meta { source },
                ..
            }) if source == OUTPUT_LIMIT_RESUME_SOURCE => {
                self.pending = self.last_text_row(edit, turn);
                None
            }
            Message::Assistant(_) => {
                let joins = matches!(first, Some(Part::Text { .. }));
                for (_, streamed) in std::mem::take(&mut self.continued) {
                    if !joins {
                        write_joined(edit, &streamed, &streamed.response, "", DONE);
                    }
                }
                self.pending.take().filter(|_| joins)
            }
            _ => {
                self.pending = None;
                None
            }
        }
    }

    /// 续写的最终正文（`message()` 的正文部分）：行 = 续写之前的正文 + 这一条的。
    pub(super) fn join(&self, edit: &mut Edit<'_>, to: &Continuation, message: &str, text: &str) {
        write_joined(edit, to, message, text, DONE);
    }

    /// 流式的一段正文（`delta()` 调）：接进了就返回真。第一段（这条消息还没有流式的行）且记着续写时，先把行放回续写之前的
    /// 正文再接（同一次续写用新的 message id 重发时，盖掉上一次的半截）。
    pub(super) fn joined_stream(
        &mut self,
        edit: &mut Edit<'_>,
        message: &MessageId,
        chunk: &str,
    ) -> bool {
        if let Some(to) = self.continued.get(message) {
            edit.append(ROWS, &to.row, keys::TEXT, chunk);
            return true;
        }
        if self.streams.contains_key(message) {
            return false;
        }
        let Some(to) = self.pending.clone() else {
            return false;
        };
        write_joined(edit, &to, message.as_str(), chunk, STREAMING);
        self.continued.insert(message.clone(), to);
        true
    }

    /// 回合结束：还没等到 `MessageDone` 的续写（请求失败）放回续写之前的正文（冷读里没有那条消息）。
    pub(super) fn settle_continuations(&mut self, edit: &mut Edit<'_>) {
        self.pending = None;
        for (_, streamed) in std::mem::take(&mut self.continued) {
            write_joined(edit, &streamed, &streamed.response, "", DONE);
        }
    }

    /// 最后一行是这个回合的正文行时，它就是续写要接进的行。
    fn last_text_row(&self, edit: &Edit<'_>, turn: &str) -> Option<Continuation> {
        let (_, id) = self.order.last()?;
        let fields = edit.get(ROWS, id)?;
        (rows::text(fields, keys::KIND) == kinds::ASSISTANT_TEXT
            && rows::text(fields, keys::TURN_ID) == turn)
            .then(|| Continuation {
                row: id.clone(),
                base: rows::text(fields, keys::TEXT).to_owned(),
                response: rows::text(fields, keys::RESPONSE).to_owned(),
            })
    }
}

/// 行的正文 = 续写之前的 + `text`，`state` 与 `assistantResponseId` 换成这一条的。
fn write_joined(edit: &mut Edit<'_>, to: &Continuation, response: &str, text: &str, state: &str) {
    let Some(mut fields) = edit.get(ROWS, &to.row).cloned() else {
        return;
    };
    fields.insert(keys::TEXT.to_owned(), json!(format!("{}{text}", to.base)));
    fields.insert(keys::STATE.to_owned(), json!(state));
    fields.insert(keys::STABLE.to_owned(), json!(state == DONE));
    fields.insert(keys::RESPONSE.to_owned(), json!(response));
    edit.put(ROWS, &to.row, fields);
}

#[cfg(test)]
#[path = "projection_continue_tests.rs"]
mod tests;
