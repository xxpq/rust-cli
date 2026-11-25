//! 开发者面板的调试记录（protocol-v5.md §43.2 的 `session.debug`）：actor 里每个会话一份，由通道转来的内核事件喂（`ModelCalled`、
//! `ModelNotice`），只在内存。与 Node 跟随 record 的旁路记录同义：记的是本进程观察到的请求，由视图重建投影不清它。

use std::collections::{BTreeMap, VecDeque};

use rust_cli_base::units;

use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::{ModelNotice, QuerySource};
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use serde_json::{Value, json};

use crate::consts::{DEBUG_NETWORK_ENTRIES, DEBUG_ROUNDS};
use crate::generated::session_v1::{
    QuerySessionDebugResult, QuerySessionDebugResultCache,
    QuerySessionDebugResultNetworkEntriesItem as Entry,
    QuerySessionDebugResultNetworkEntriesItemStatusType as EntryStatus,
    QuerySessionDebugResultRoundsItem as Round, QuerySessionDebugResultRoundsItemUsage,
};

/// 失败的请求的 `reason`：被用户中断的与别的失败分开（Node 的条目也带原因）。
const REASON_CANCELLED: &str = "cancelled";
const REASON_FAILED: &str = "failed";

/// token 数与毫秒时刻 → 线上的数字（生成的类型是 `f64`）。
#[expect(
    clippy::cast_precision_loss,
    reason = "token 数与毫秒时间远小于 2^53，f64 能精确表示"
)]
fn number(value: u64) -> f64 {
    value as f64
}

/// 主回合请求的缓存累计（Node 的 `cache`）。
#[derive(Debug, Default)]
struct Cache {
    requests: u64,
    input: u64,
    read: u64,
    /// 有一轮没有用量：命中率不可知。
    unknown: bool,
}

/// 一个会话的调试记录。
#[derive(Debug, Default)]
pub(crate) struct DebugLog {
    /// 会话的 trace（视图的 `meta.trace`）：每条网络条目带它。
    trace: String,
    rounds: VecDeque<Round>,
    network: VecDeque<Entry>,
    cache: Option<Cache>,
}

fn entry(seq: u64, trace: &str, recorded_at: u64, status_type: EntryStatus) -> Entry {
    Entry {
        event_key: seq.to_string(),
        trace_id: trace.to_owned(),
        recorded_at: number(recorded_at),
        status_type,
        request_id: None,
        provider_id: None,
        model_id: None,
        provider_kind: None,
        transport: None,
        base_url: None,
        query_source: None,
        query_id: None,
        timestamp: None,
        attempt: None,
        max_attempts: None,
        next_attempt: None,
        retryable: None,
        status_code: None,
        duration_ms: None,
        delay_ms: None,
        idle_ms: None,
        timeout_ms: None,
        reason: None,
        message: None,
        // rust 的模型适配器不对外给请求头与响应头（§43.7 第 3 项）
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_header_count: 0.0,
        response_header_count: 0.0,
    }
}

fn push<T>(items: &mut VecDeque<T>, item: T, limit: usize) {
    if items.len() == limit {
        items.pop_front();
    }
    items.push_back(item);
}

impl DebugLog {
    /// 视图到了（物化、激活、改写历史）：记下会话的 trace，已有的记录不清。
    pub(crate) fn traced(&mut self, trace: &str) {
        trace.clone_into(&mut self.trace);
    }

    /// 通道转来的一个内核事件。
    pub(crate) fn observe(&mut self, event: &SequencedEvent) {
        match &event.event {
            KernelEvent::ModelCalled { call } => self.called(event.seq, call),
            KernelEvent::ModelNotice { notice, at } => self.noticed(event.seq, notice, *at),
            _ => {}
        }
    }

    fn called(&mut self, seq: u64, call: &ModelCall) {
        let (status_type, reason) = match call.status {
            CallStatus::Completed => (EntryStatus::ModelRequestCompleted, None),
            CallStatus::Error => (EntryStatus::ModelRequestFailed, Some(REASON_FAILED)),
            CallStatus::Cancelled => (EntryStatus::ModelRequestFailed, Some(REASON_CANCELLED)),
        };
        let mut item = entry(seq, &self.trace, call.completed_at_ms, status_type);
        item.request_id = Some(call.message.as_str().to_owned());
        item.provider_id = Some(call.selection.provider.clone());
        item.model_id = Some(call.selection.model.clone());
        item.query_source = Some(call.source.node_name().to_owned());
        item.duration_ms = Some(number(
            call.completed_at_ms.saturating_sub(call.started_at_ms),
        ));
        item.reason = reason.map(str::to_owned);
        item.message = call.error.as_ref().map(ToString::to_string);
        push(&mut self.network, item, DEBUG_NETWORK_ENTRIES);
        if call.source == QuerySource::MainThread && call.status == CallStatus::Completed {
            self.round(seq, call);
        }
    }

    /// 一个完成的主回合请求（Node 的 rounds 与 cache）：输入是 Node 的归一化输入（输入 + 缓存读 + 缓存写，usage.md §2.2）。
    fn round(&mut self, seq: u64, call: &ModelCall) {
        let usage = call.usage.as_ref();
        let input = usage.map(|usage| {
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens
        });
        let read = usage.map(|usage| usage.cache_read_input_tokens);
        let output = usage.map(|usage| usage.output_tokens);
        let cache = self.cache.get_or_insert_with(Cache::default);
        cache.requests += 1;
        cache.input += input.unwrap_or_default();
        cache.read += read.unwrap_or_default();
        cache.unknown |= usage.is_none();
        let generation = call
            .first_content_at_ms
            .map(|first| call.completed_at_ms.saturating_sub(first))
            .filter(|ms| *ms > 0);
        let round = Round {
            event_key: seq.to_string(),
            request_id: call.message.as_str().to_owned(),
            request_index: number(cache.requests),
            recorded_at: number(call.completed_at_ms),
            usage: QuerySessionDebugResultRoundsItemUsage {
                input_tokens: input.map(number),
                output_tokens: output.map(number),
                total_tokens: input
                    .zip(output)
                    .map(|(input, output)| number(input + output)),
                reasoning_tokens: None,
                cached_input_tokens: read.map(number),
                cached_write_input_tokens: usage
                    .map(|usage| number(usage.cache_creation_input_tokens)),
            },
            hit_rate: input
                .zip(read)
                .filter(|(input, _)| *input > 0)
                .map(|(input, read)| number(read) / number(input)),
            generation_duration_ms: generation.map(number),
            tokens_per_second: output
                .zip(generation)
                .map(|(output, ms)| number(output) * units::MS_PER_SECOND_F64 / number(ms)),
        };
        push(&mut self.rounds, round, DEBUG_ROUNDS);
    }

    fn noticed(&mut self, seq: u64, notice: &ModelNotice, at: u64) {
        let item = match notice {
            ModelNotice::Retrying {
                attempt,
                max_retries,
                delay_ms,
                error,
            } => {
                let mut item = entry(seq, &self.trace, at, EntryStatus::ModelRetryScheduled);
                item.attempt = Some(f64::from(*attempt));
                item.max_attempts = Some(f64::from(*max_retries));
                item.delay_ms = Some(number(*delay_ms));
                item.message = Some(error.to_string());
                item
            }
            ModelNotice::Stalled { .. } => {
                entry(seq, &self.trace, at, EntryStatus::ModelStreamStalled)
            }
            ModelNotice::Resumed => return,
        };
        push(&mut self.network, item, DEBUG_NETWORK_ENTRIES);
    }

    /// `session.debug` 的结果。
    pub(crate) fn snapshot(&self, session: &str) -> Value {
        json!(QuerySessionDebugResult {
            session_id: session.to_owned(),
            rounds: self.rounds.iter().cloned().collect(),
            network_entries: self.network.iter().cloned().collect(),
            cache: self
                .cache
                .as_ref()
                .map(|cache| QuerySessionDebugResultCache {
                    hit_rate_request_count: number(cache.requests),
                    total_input_tokens: number(cache.input),
                    total_cache_read_tokens: number(cache.read),
                    hit_rate: (!cache.unknown && cache.input > 0)
                        .then(|| number(cache.read) / number(cache.input)),
                }),
        })
    }
}

#[cfg(test)]
#[path = "debug_tests.rs"]
mod tests;
