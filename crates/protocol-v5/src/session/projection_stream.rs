//! 流式的增量落到行上（protocol-v5.md §15.3.5，K-I3；从 projection.rs 分出，800 行上限）：文字、思考与工具输入的增量建行或接到行上。

use super::*;

impl Projection {
    pub(super) fn delta(
        &mut self,
        edit: &mut Edit<'_>,
        turn: &str,
        message: &MessageId,
        index: usize,
        tool_use: Option<&ToolUseId>,
        delta: &BlockDelta,
    ) {
        if let BlockDelta::Text(chunk) = delta
            && self.joined_stream(edit, message, chunk)
        {
            return;
        }
        let created = message.created_ms();
        let stream = self.streams.entry(message.clone()).or_default();
        stream.turn = turn.to_owned();
        let (id, kind, path, chunk) = match delta {
            BlockDelta::Text(chunk) => (
                message.as_str().to_owned(),
                kinds::ASSISTANT_TEXT,
                keys::TEXT,
                chunk,
            ),
            BlockDelta::Thinking(chunk) => {
                let next = stream.reasoning.len();
                let k = *stream.reasoning.entry(index).or_insert(next);
                (
                    rows::reasoning_id(message, k),
                    kinds::REASONING,
                    keys::TEXT,
                    chunk,
                )
            }
            BlockDelta::InputJson(chunk) => match tool_use {
                Some(call) => (
                    call.as_str().to_owned(),
                    kinds::TOOL_CALL,
                    keys::INPUT_TEXT,
                    chunk,
                ),
                None => return,
            },
            BlockDelta::Signature(_) => return,
        };
        stream.rows.insert(id.clone());
        if edit.get(ROWS, &id).is_some() {
            edit.append(ROWS, &id, path, chunk);
            return;
        }
        let seq = self.next(&id);
        let mut fields = if kind == kinds::TOOL_CALL {
            self.open_tools.insert(id.clone(), turn.to_owned());
            rows::tool_call(
                tool_status::INPUT_STREAMING,
                message.as_str(),
                (turn, seq, created),
            )
        } else {
            rows::answer(kind, "", STREAMING, message.as_str(), (turn, seq, created))
        };
        fields.insert(path.to_owned(), json!(chunk));
        edit.put(ROWS, &id, fields);
    }
}
