//! 两阶段的请求与结论（docs/specs/rust-cli/auto-classifier.md §15.6、§15.7）。
//!
//! 第一阶段只要一个分数或 yes/no，判放行就结束；否则第二阶段带 `<thinking>` 再判一次。回复解析不了（不是拒绝）时在阶段的时限里重发。
//! 时限只是请求的时限：超时等于这次请求失败。

use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::classifier::{ClassifierVerdict, NoVerdict};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::message::{ContentBlock, StopReason};
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, CacheControl, ModelError, ModelNotice, ModelPort,
    ModelRequest, ModelSelection, ModelStreamItem, SystemBlock,
};
use rust_cli_kernel_api::side_request::{Requester, SideReply, SideSource};
use tokio_util::sync::CancellationToken;

use crate::choice;
use crate::limits::{
    self, API_RETRIES, RESENDS, STAGE_ONE_MAX_TOKENS, STAGE_TWO_MAX_TOKENS, StageLimits,
};
use crate::parse;
use crate::prompt::{Format, RuleNames, Stage};
use crate::texts;

/// 摘录的开头与结尾。
const TRANSCRIPT_OPEN: &str = "<transcript>\n";
const TRANSCRIPT_CLOSE: &str = "</transcript>\n";
/// 缓存断点的种类。
const EPHEMERAL: &str = "ephemeral";

/// 一次分类的请求材料：两个阶段只差后缀、输出上限与停止序列。
pub(crate) struct Job<'a> {
    pub(crate) format: Format,
    pub(crate) system: String,
    pub(crate) names: &'a RuleNames,
    /// 历史的几块（可以没有；判过的调用各自成块，auto-classifier.md §16.15）与动作那一行。
    pub(crate) history: Vec<String>,
    pub(crate) action: String,
    pub(crate) requester: Requester,
    pub(crate) cancel: CancellationToken,
    /// 第一阶段一次尝试的时限（按摘录的长度）。
    pub(crate) attempt: Duration,
    /// 请求时限的计时（毫秒）。
    pub(crate) clock: &'a dyn Clock,
}

/// 一次请求失败：模型端口的错误，或者到了请求时限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failed {
    Model(ModelError),
    TimedOut,
}

/// 判的结果：有结论，或者第一阶段的请求失败（模型不可用；探测按它退回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Judged {
    Verdict(ClassifierVerdict),
    FirstStageFailed(Failed),
}

fn ephemeral() -> Option<CacheControl> {
    Some(CacheControl {
        kind: EPHEMERAL.to_owned(),
        ttl: None,
        scope: None,
    })
}

fn text(text: &str, cache_control: Option<CacheControl>) -> ApiContent {
    ApiContent {
        block: ContentBlock::Text {
            text: text.to_owned(),
        },
        cache_control,
    }
}

impl Job<'_> {
    /// 这个阶段的请求：一块提示、一条 user 消息（`<transcript>`、历史、动作、`</transcript>`、后缀）。
    pub(crate) fn request(&self, selection: &ModelSelection, stage: Stage) -> ModelRequest {
        let mut content = vec![text(TRANSCRIPT_OPEN, None)];
        // 历史的最后一块带缓存标记（前面的块是上一次请求的前缀）。
        let last = self.history.len().checked_sub(1);
        for (index, block) in self.history.iter().enumerate() {
            let cache = (Some(index) == last).then(ephemeral).flatten();
            content.push(text(block, cache));
        }
        content.push(text(&self.action, ephemeral()));
        content.push(text(TRANSCRIPT_CLOSE, None));
        let suffix = self.format.suffix(stage);
        if !suffix.trim().is_empty() {
            content.push(text(suffix, None));
        }
        let message = ApiMessage {
            role: ApiRole::User,
            content,
            output_config: None,
        };
        let system = vec![SystemBlock {
            text: self.system.clone(),
            cache_control: ephemeral(),
        }];
        let (thinking, extra) = choice::thinking(&selection.model);
        let max_tokens = match stage {
            Stage::One => STAGE_ONE_MAX_TOKENS,
            Stage::Two => STAGE_TWO_MAX_TOKENS,
        } + extra;
        let mut request = ModelRequest::side(
            selection.clone(),
            SideSource::AutoMode,
            system,
            vec![message],
            max_tokens,
            self.requester.clone(),
        );
        request.stop_sequences = self.format.stop(stage);
        request.thinking = thinking;
        request.options.max_retries = Some(API_RETRIES);
        // 修 bug（C2 合入 MO-IMPL 时查出，auto-classifier.md §16.16）：配置路上平台层的 maxOutputTokens 补丁以配置的上限改写 max_tokens，
        // 分类器的 64 / 8192 变成 128000。同 WebSearch 的旁路请求，把这次请求自己的上限交给补丁。
        request.options.provider_max_output_tokens = Some(max_tokens);
        request
    }

    /// 回复能不能解析出结论（不看结论是什么）。
    fn parses(&self, reply: &SideReply) -> bool {
        match self.format {
            Format::Severity { .. } => parse::severity(&reply.text(), reply.stop_reason).is_some(),
            Format::Block => parse::block(&reply.text()).is_some(),
        }
    }

    /// 拦下了没有；解析不了时 None。
    fn blocked(&self, reply: &SideReply, stage: Stage) -> Option<bool> {
        match self.format {
            Format::Severity { t1, t2 } => {
                let limit = if stage == Stage::One { t1 } else { t2 };
                parse::severity(&reply.text(), reply.stop_reason).map(|score| score > limit)
            }
            Format::Block => parse::block(&reply.text()),
        }
    }

    /// 拦下时给模型的理由。
    fn reason(&self, reply: &SideReply, stage: Stage) -> String {
        let text = reply.text();
        let fallback = match (self.format, stage) {
            (_, Stage::One) => texts::BLOCKED_FAST,
            (Format::Severity { .. }, Stage::Two) => texts::BLOCKED,
            (Format::Block, Stage::Two) => texts::NO_REASON,
        };
        let found = match self.format {
            Format::Severity { .. } => parse::category(&text, self.names),
            Format::Block => parse::reason(&text),
        };
        found.unwrap_or_else(|| fallback.to_owned())
    }

    /// 发一个阶段：解析不了且不是拒绝时，在重发的窗口里最多再发 [`RESENDS`] 次。
    async fn stage(
        &self,
        model: &dyn ModelPort,
        selection: &ModelSelection,
        stage: Stage,
        limits: StageLimits,
    ) -> Result<SideReply, Failed> {
        let started = self.clock.now_ms();
        let deadline = started.saturating_add(millis(limits.ceiling));
        let mut reply = self
            .send(
                model,
                self.request(selection, stage),
                limits.attempt,
                deadline,
            )
            .await?;
        let mut resent = 0;
        while resent < RESENDS
            && !self.parses(&reply)
            && !parse::refused(&reply)
            && self.clock.now_ms().saturating_sub(started) < millis(limits.resend_window)
        {
            resent += 1;
            reply = self
                .send(
                    model,
                    self.request(selection, stage),
                    limits.attempt,
                    deadline,
                )
                .await?;
        }
        Ok(reply)
    }

    /// 两个阶段与结论（§15.7 的表）。
    pub(crate) async fn judge(&self, model: &dyn ModelPort, selection: &ModelSelection) -> Judged {
        // 关思考的档位照 provider 的配置（docs/specs/rust-cli/model-options.md §12.4）：有「关」这一档时用它，否则第一档。
        // 修 bug：以前只写 `thinking: {type: "disabled"}`，平台层的补丁按会话的档位（没有时最后一档）求值，把 thinking 改回开着。
        let offered = model.reasoning_levels(selection).await;
        let selection = &ModelSelection {
            reasoning: rust_cli_prompts::models::quiet_reasoning(
                selection.reasoning.as_deref(),
                offered.as_deref(),
            ),
            ..selection.clone()
        };
        let one = match self
            .stage(
                model,
                selection,
                Stage::One,
                limits::stage_one(self.attempt),
            )
            .await
        {
            Ok(reply) => reply,
            Err(failed) => return Judged::FirstStageFailed(failed),
        };
        let first = self.blocked(&one, Stage::One);
        if first == Some(false) {
            return Judged::Verdict(ClassifierVerdict::Allow);
        }
        let first_refused = first.is_none() && one.stop_reason == Some(StopReason::Refusal);
        let first_reason = (first == Some(true)).then(|| self.reason(&one, Stage::One));
        let two = match self
            .stage(
                model,
                selection,
                Stage::Two,
                limits::stage_two(self.attempt),
            )
            .await
        {
            Ok(reply) => reply,
            Err(failed) => {
                return Judged::Verdict(self.second_stage_failed(failed, first_refused));
            }
        };
        let verdict = match self.blocked(&two, Stage::Two) {
            Some(false) => ClassifierVerdict::Allow,
            Some(true) => ClassifierVerdict::Block {
                reason: self.reason(&two, Stage::Two),
            },
            None => unparsed(&two, first_reason, first_refused),
        };
        Judged::Verdict(verdict)
    }

    /// 第二阶段的请求失败：取消、第一阶段是拒绝、对话太长各有结论；其余照第一阶段的回复拦下。
    fn second_stage_failed(&self, failed: Failed, first_refused: bool) -> ClassifierVerdict {
        if self.cancel.is_cancelled() {
            return ClassifierVerdict::NoVerdict(NoVerdict::Cancelled);
        }
        if first_refused {
            return ClassifierVerdict::NoVerdict(NoVerdict::Refused);
        }
        let retry_after = match &failed {
            Failed::Model(ModelError::PromptTooLong { .. }) => {
                return ClassifierVerdict::NoVerdict(NoVerdict::TranscriptTooLong);
            }
            // 端点的内容安全拦截同拒绝回答（第一阶段的失败也这样认）
            Failed::Model(ModelError::ContentBlocked { .. }) => {
                return ClassifierVerdict::NoVerdict(NoVerdict::Refused);
            }
            Failed::Model(ModelError::RateLimited { retry_after_ms, .. }) => *retry_after_ms,
            Failed::Model(_) | Failed::TimedOut => None,
        };
        let reason = retry_after.map_or_else(
            || texts::STAGE_TWO_ERROR.to_owned(),
            texts::stage_two_error_waiting,
        );
        ClassifierVerdict::Block { reason }
    }
}

/// 第二阶段解析不了：第一阶段判拦下而第二阶段拒绝回答时照第一阶段拦下；其余只要有一个阶段是拒绝就是拒绝，否则是解析不了。
///
/// 修 bug（C2，auto-classifier.md §16.3）：以前“第二阶段是拒绝”用的是 `parse::refused`（文字为空也算），空回复成了安全拦截：给模型的
/// 是拦截的那一句、不计数。安全拦截只认 `stop_reason` 是 refusal；文字为空只是不重发（重发的条件仍用 `parse::refused`）。
fn unparsed(
    two: &SideReply,
    first_reason: Option<String>,
    first_refused: bool,
) -> ClassifierVerdict {
    let two_refused = two.stop_reason == Some(StopReason::Refusal);
    if two_refused && let Some(reason) = first_reason {
        return ClassifierVerdict::Block { reason };
    }
    if first_refused || two_refused {
        ClassifierVerdict::NoVerdict(NoVerdict::Refused)
    } else {
        ClassifierVerdict::NoVerdict(NoVerdict::Unparseable)
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl Job<'_> {
    /// 发一次请求并收齐回复。端口每发一次重试通知，尝试的计时重新开始；整个请求不超过 `deadline`（时钟的毫秒）。
    async fn send(
        &self,
        model: &dyn ModelPort,
        request: ModelRequest,
        attempt: Duration,
        deadline: u64,
    ) -> Result<SideReply, Failed> {
        let mut stream = model.stream_with_notices(request, self.cancel.clone());
        let mut reply = SideReply::default();
        let mut until = self
            .clock
            .now_ms()
            .saturating_add(millis(attempt))
            .min(deadline);
        loop {
            let left = Duration::from_millis(until.saturating_sub(self.clock.now_ms()));
            match tokio::time::timeout(left, stream.next()).await {
                Err(_) => return Err(Failed::TimedOut),
                Ok(None) => return Ok(reply),
                Ok(Some(Err(error))) => return Err(Failed::Model(error)),
                Ok(Some(Ok(ModelStreamItem::Event(event)))) => reply.push(event),
                Ok(Some(Ok(ModelStreamItem::Notice(ModelNotice::Retrying { .. })))) => {
                    until = self
                        .clock
                        .now_ms()
                        .saturating_add(millis(attempt))
                        .min(deadline);
                }
                Ok(Some(Ok(ModelStreamItem::Notice(
                    ModelNotice::Stalled { .. } | ModelNotice::Resumed,
                )))) => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "stages_tests.rs"]
mod tests;
