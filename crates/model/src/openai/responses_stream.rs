//! OpenAI Responses 的 SSE 流（docs/specs/rust-cli/model.md §10.3）。
//!
//! 按 `data.type` 分发：`response.created` 产出 `MessageStart`；`output_item.added` / `.done` 圈定每个输出项，
//! 函数调用一项一个 tool_use 块；正文（`output_text` / `refusal`）与推理摘要（`reasoning_summary_text`、
//! `reasoning_text`）的增量进当前流；`response.completed` / `response.incomplete` 写出停止原因与用量后结束。
//! `error` 与 `response.failed` 按错误结束。不认识的事件与输出项（服务端工具等）忽略。
//! 函数调用的参数收到就写（工作线 F147），`output_item.done` 的完整参数补上没写出的部分，见 [`ResponsesStream::done`]。
//! 输出项按 `output_index` 登记与结束；增量按 `item_id` 找登记的项，见 [`ResponsesStream::delta`]。

use std::collections::{BTreeMap, VecDeque};

use rust_cli_kernel_api::message::{StopReason, Usage};
use rust_cli_kernel_api::model::ModelEvent;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::sse::SseEvent;
use serde_json::{Value, json};

use crate::errors::{self, Finish, Vendor, Verdict};
use crate::openai::blocks::{Blocks, Flow, number, protocol, take_str, usage};
use crate::protocol::StreamDecoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Message,
    Reasoning,
    Call,
    Skipped,
}

#[derive(Debug)]
struct Output {
    /// `output_item.added` 的 `item.id`：增量按它找这一项。
    id: String,
    kind: Kind,
    /// 收到过增量；没有增量的项在 `.done` 时从完整内容补出。
    streamed: bool,
    /// 函数调用已经写出的参数（`added` 里的与增量），done 时与完整参数比较。
    arguments: String,
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesStream {
    /// 错误与结束原因经这个厂商的分类器（provider-errors.md §2.2，修复线 F113）。
    vendor: Vendor,
    started: bool,
    finished: bool,
    blocks: Blocks,
    /// 按 `output_index` 找输出项。
    outputs: BTreeMap<u64, Output>,
}

fn output_index(event: &Value) -> Result<u64, Verdict> {
    event
        .get("output_index")
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol("output event without an output_index"))
}

/// 完整输出项里的文本：消息的 `content[*].text` / `refusal`，推理的 `summary[*].text`。
fn item_text(item: &Value, field: &str) -> String {
    item.get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            part.get("text")
                .or_else(|| part.get("refusal"))
                .and_then(Value::as_str)
        })
        .collect()
}

impl ResponsesStream {
    pub(crate) fn new(vendor: Vendor) -> Self {
        Self {
            vendor,
            ..Self::default()
        }
    }

    /// 流里的错误对象经厂商的分类器（修复线 F113）。
    fn error(&self, value: &Value) -> Verdict {
        errors::classify_stream_error(
            self.vendor,
            WireProtocol::OpenAiResponses,
            value,
            &value.to_string(),
        )
    }

    fn start(&mut self, event: &mut Value, out: &mut VecDeque<ModelEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        let mut response = event
            .get_mut("response")
            .map(Value::take)
            .unwrap_or_default();
        out.push_back(ModelEvent::MessageStart {
            id: take_str(&mut response, "id").unwrap_or_default(),
            model: take_str(&mut response, "model").unwrap_or_default(),
            usage: Usage::default(),
        });
    }

    fn added(&mut self, mut event: Value, out: &mut VecDeque<ModelEvent>) -> Result<(), Verdict> {
        let key = output_index(&event)?;
        if self.outputs.contains_key(&key) {
            return Err(protocol(format!("output item {key} added twice")));
        }
        let mut item = event.get_mut("item").map(Value::take).unwrap_or_default();
        let id = take_str(&mut item, "id").unwrap_or_default();
        let mut arguments = String::new();
        let kind = match item.get("type").and_then(Value::as_str) {
            Some("message") => Kind::Message,
            Some("reasoning") => Kind::Reasoning,
            Some("function_call") => {
                let id = take_str(&mut item, "call_id").unwrap_or_default();
                let name = take_str(&mut item, "name").unwrap_or_default();
                self.blocks.tool_start(key, id, name, out)?;
                arguments = take_str(&mut item, "arguments").unwrap_or_default();
                self.blocks.tool_arguments(key, arguments.clone(), out)?;
                Kind::Call
            }
            other => {
                tracing::warn!(item_type = ?other, "skipping an unsupported Responses output item");
                Kind::Skipped
            }
        };
        self.outputs.insert(
            key,
            Output {
                id,
                kind,
                streamed: false,
                arguments,
            },
        );
        Ok(())
    }

    /// 修 bug（model.md §10.3）：以前增量按 `output_index` 找输出项，只给 `item_id` 的增量报 `Protocol`；交出边界之前的
    /// `Protocol` 走非流式回退，回退的请求拿到的还是流、解析失败，回合以错误结束，一个字都没显示。Node 用的 SDK 认正文与
    /// 推理的增量只看 `item_id`，官方端点两样都给；按 `item_id` 找，Node 收得下的增量这里都收得下。
    fn delta(
        &mut self,
        mut event: Value,
        expected: Kind,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), Verdict> {
        let id = take_str(&mut event, "item_id")
            .ok_or_else(|| protocol("delta event without an item_id"))?;
        // 工具参数的增量收到就写（工作线 F147）：写进这个输出项的块，块按 `output_index` 登记（`added`）。
        let (key, output) = self
            .outputs
            .iter_mut()
            .find(|(_, output)| output.id == id)
            .map(|(key, output)| (*key, output))
            .ok_or_else(|| protocol(format!("delta for unknown output item {id}")))?;
        if output.kind == Kind::Skipped {
            return Ok(());
        }
        if output.kind != expected {
            return Err(protocol(format!("delta does not match output item {id}")));
        }
        output.streamed = true;
        let delta = take_str(&mut event, "delta").unwrap_or_default();
        match expected {
            Kind::Message => self.blocks.flow(Flow::Text, delta, out),
            Kind::Reasoning => self.blocks.flow(Flow::Thinking, delta, out),
            Kind::Call => {
                output.arguments.push_str(&delta);
                self.blocks.tool_arguments(key, delta, out)?;
            }
            Kind::Skipped => {}
        }
        Ok(())
    }

    fn done(&mut self, event: Value, out: &mut VecDeque<ModelEvent>) -> Result<(), Verdict> {
        let key = output_index(&event)?;
        let output = self
            .outputs
            .get_mut(&key)
            .ok_or_else(|| protocol(format!("unknown output item {key} done")))?;
        let item = &event["item"];
        match output.kind {
            // 修 bug（修复线 F101，model.md §10.3）：以前这里只结束块；有的供应商不发增量，完整参数只在 done 里，调用的 input
            // 全是 `{}`，工具报必填参数缺失。done 的 `item.arguments` 为准（Node 用的 SDK 同样如此）；`function_call_arguments.done`
            // 与它重复，不处理。工作线 F147 起增量收到就写（`-p` 的 partial 消息要逐段显示），这里只补没写出的后缀，没写过就是
            // 全部；没有完整参数时按已写的算。不是后缀（不合规的网关）时写出的收不回，保留已写的（待确认，headless.md §10）。
            // 后缀为空也调用：块已结束（同一项 done 两次）时它报 Protocol 错误。
            Kind::Call => {
                let written = std::mem::take(&mut output.arguments);
                let full = item["arguments"].as_str().unwrap_or(written.as_str());
                let rest = full.strip_prefix(written.as_str()).unwrap_or_else(|| {
                    tracing::warn!(
                        output_index = key,
                        written = written.len(),
                        complete = full.len(),
                        "Responses function call arguments differ from the streamed deltas; keeping the deltas"
                    );
                    ""
                });
                self.blocks.tool_arguments(key, rest.to_owned(), out)?;
                self.blocks.tool_stop(key, out);
            }
            Kind::Message | Kind::Reasoning => {
                let (flow, field) = if output.kind == Kind::Message {
                    (Flow::Text, "content")
                } else {
                    (Flow::Thinking, "summary")
                };
                if !output.streamed {
                    self.blocks.flow(flow, item_text(item, field), out);
                }
                self.blocks.end_flow(out);
            }
            Kind::Skipped => {}
        }
        Ok(())
    }

    fn completed(&mut self, event: &Value, out: &mut VecDeque<ModelEvent>) -> Result<(), Verdict> {
        let response = &event["response"];
        let reported = &response["usage"];
        let cached = number(&reported["input_tokens_details"], "cached_tokens");
        let usage = usage(
            number(reported, "input_tokens"),
            cached,
            number(reported, "output_tokens"),
        );
        // 不完整的原因经厂商的分类器换算（修复线 F113：以前是这里自己的两项）。
        let stop = match response["incomplete_details"]["reason"].as_str() {
            Some(reason) => match errors::classify_finish(self.vendor, reason) {
                Finish::Stop(reason) => reason,
                Finish::Failed(verdict) => return Err(verdict),
            },
            None if self.blocks.has_tools() => StopReason::ToolUse,
            None => StopReason::EndTurn,
        };
        self.finished = true;
        self.blocks.finish(Some(stop), usage, out);
        Ok(())
    }
}

impl StreamDecoder for ResponsesStream {
    fn decode(&mut self, event: SseEvent, out: &mut VecDeque<ModelEvent>) -> Result<(), Verdict> {
        if self.finished {
            return Ok(());
        }
        let mut value: Value = serde_json::from_str(&event.data)
            .map_err(|error| protocol(format!("invalid event JSON: {error}")))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match kind.as_str() {
            "error" => return Err(self.error(&value)),
            "response.failed" => {
                let error = value["response"]["error"].take();
                return Err(self.error(&json!({ "error": error })));
            }
            "response.created" | "response.in_progress" => self.start(&mut value, out),
            _ if !self.started => {
                return Err(protocol(format!("{kind} before response.created")));
            }
            "response.output_item.added" => self.added(value, out)?,
            "response.output_text.delta" | "response.refusal.delta" => {
                self.delta(value, Kind::Message, out)?;
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.delta(value, Kind::Reasoning, out)?;
            }
            "response.function_call_arguments.delta" => self.delta(value, Kind::Call, out)?,
            "response.output_item.done" => self.done(value, out)?,
            "response.completed" | "response.incomplete" => self.completed(&value, out)?,
            _ => {}
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
#[path = "responses_stream_tests.rs"]
mod tests;
