//! 自动模式的分类器（docs/specs/rust-cli/auto-classifier.md §15）：判断一个要询问的动作能不能在自动模式下直接执行。
//!
//! 实现 kernel-api 的 `AutoClassifier`：按（会话，代理）排队，把这一步的对话摘录与动作交给模型（两阶段的旁路请求），解析回复，
//! 给出结论。只经 `ModelPort` 发请求；判决的接法与给模型的文字在 policy。

mod choice;
mod lanes;
mod limits;
mod parse;
mod prompt;
mod stages;
mod texts;
mod transcript;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::js::string::utf16_len;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::classifier::{
    AutoClassifier, ClassifierVerdict, ClassifyRequest, NoVerdict, Unavailable, UnavailableCause,
};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::default_model::DefaultModel;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{ModelError, ModelPort, ModelSelection};
use rust_cli_kernel_api::side_request::Requester;
use rust_cli_kernel_api::tool::ToolUseContext;

use crate::choice::{Choice, Probe, Probes};
use crate::lanes::Lanes;
use crate::prompt::{Format, RuleNames};
use crate::stages::{Failed, Job, Judged};

pub struct Classifier {
    model: Arc<dyn ModelPort>,
    /// 请求时限的计时。
    clock: Arc<dyn Clock>,
    /// 会话的视图给不出主模型时用的缺省模型（同 WebFetch 的摘要）。
    fallback: DefaultModel,
    probes: Probes,
    lanes: Lanes,
    /// 判过的调用（tool_use ID）：摘录里它们各自成一块，下一次请求的前缀与上一次的相同（auto-classifier.md §16.15）。只增不减：
    /// 一个 ID 只属于一次调用。
    classified: Mutex<HashSet<ToolUseId>>,
}

impl Classifier {
    pub fn new(model: Arc<dyn ModelPort>, clock: Arc<dyn Clock>, fallback: DefaultModel) -> Self {
        Self {
            model,
            clock,
            fallback,
            probes: Probes::default(),
            lanes: Lanes::default(),
            classified: Mutex::default(),
        }
    }

    /// 会话现在的主模型：开判时读一次，两个阶段与退回都用它。
    async fn main_model(&self, context: &ToolUseContext) -> ModelSelection {
        context
            .session_ops
            .view()
            .await
            .and_then(|view| view.selection)
            .unwrap_or_else(|| self.fallback.current())
    }

    /// 判一次；这一次是探测时记下结论，探测失败时在剩下的时限里用主模型再判一次。
    async fn judge(
        &self,
        job: &Job<'_>,
        main: &ModelSelection,
        choice: Choice,
    ) -> (Judged, String) {
        let started = self.clock.now_ms();
        let judged = job.judge(self.model.as_ref(), &choice.selection).await;
        if !choice.probing || job.cancel.is_cancelled() {
            return (judged, choice.selection.model);
        }
        let Judged::FirstStageFailed(failed) = &judged else {
            self.probes.settle(&main.provider, Probe::Confirmed);
            return (judged, choice.selection.model);
        };
        if matches!(failed, Failed::Model(ModelError::Authentication { .. })) {
            return (judged, choice.selection.model);
        }
        self.probes.settle(&main.provider, Probe::Demoted);
        tracing::info!(
            event = "classifier.probe.demoted",
            provider = %main.provider,
            model = %choice.selection.model,
            "the classifier model failed on this provider; using the main model"
        );
        // 两个阶段的时限之和里还有剩下的时间、主模型与分类器的模型不同时，用主模型再判一次（同一个格式）
        let budget = job.attempt + limits::STAGE_TWO_LIMIT;
        let spent = Duration::from_millis(self.clock.now_ms().saturating_sub(started));
        let Some(remaining) = budget.checked_sub(spent).filter(|left| !left.is_zero()) else {
            return (judged, choice.selection.model);
        };
        if main.model == choice.selection.model {
            return (judged, choice.selection.model);
        }
        let retried = tokio::time::timeout(remaining, job.judge(self.model.as_ref(), main))
            .await
            .unwrap_or(Judged::FirstStageFailed(Failed::TimedOut));
        (retried, main.model.clone())
    }
}

#[async_trait]
impl AutoClassifier for Classifier {
    async fn classify(&self, request: ClassifyRequest<'_>) -> ClassifierVerdict {
        let ClassifyRequest {
            tool,
            input,
            context,
            rules,
        } = request;
        let Some(action) = transcript::tool_line(tool.name(), tool.classifier_input(input)) else {
            return ClassifierVerdict::NoVerdict(NoVerdict::NothingToJudge);
        };
        let key = (context.session.clone(), context.agent.clone());
        let Some(_turn) = self.lanes.enter(key, &context.cancel).await else {
            return ClassifierVerdict::NoVerdict(NoVerdict::Cancelled);
        };
        let main = self.main_model(context).await;
        let choice = self.probes.choose(&main);
        let format = Format::for_model(&choice.selection.model);
        let system = format.system(rules);
        let names = RuleNames::from_prompt(&system);
        // 判过的调用另起一块（auto-classifier.md §16.15）：查的是这一次之前判过的；这一次的动作在发请求之前记下。
        let history = {
            let classified = self.classified.lock_unpoisoned();
            transcript::history(
                context.transcript.as_deref(),
                context.tools.as_ref(),
                &|id| classified.contains(id),
            )
        };
        self.classified
            .lock_unpoisoned()
            .insert(context.tool_use.clone());
        let chars = utf16_len(&system)
            + history.iter().map(|block| utf16_len(block)).sum::<usize>()
            + utf16_len(&action);
        let tokens = limits::estimate_tokens(chars).max(main_loop_tokens(context));
        let job = Job {
            format,
            system,
            names: &names,
            history,
            action,
            requester: Requester {
                session: context.session.clone(),
                run: Some(context.run.clone()),
                agent: context.agent.clone(),
                root: context.root.clone(),
            },
            cancel: context.cancel.clone(),
            attempt: limits::attempt_limit(tokens),
            clock: self.clock.as_ref(),
        };
        let (judged, model) = self.judge(&job, &main, choice).await;
        if context.cancel.is_cancelled() {
            return ClassifierVerdict::NoVerdict(NoVerdict::Cancelled);
        }
        let verdict = match judged {
            Judged::Verdict(verdict) => verdict,
            Judged::FirstStageFailed(failed) => first_stage_failed(failed, model),
        };
        log(tool.name(), &verdict);
        verdict
    }
}

/// 第一阶段的请求失败：对话太长、拒绝回答（端点的内容拦截）各有结论；其余是模型不可用。
fn first_stage_failed(failed: Failed, model: String) -> ClassifierVerdict {
    let (cause, retry_after_ms) = match failed {
        Failed::TimedOut => (UnavailableCause::TimedOut, None),
        Failed::Model(error) => match error {
            ModelError::Cancelled => return ClassifierVerdict::NoVerdict(NoVerdict::Cancelled),
            ModelError::PromptTooLong { .. } => {
                return ClassifierVerdict::NoVerdict(NoVerdict::TranscriptTooLong);
            }
            ModelError::ContentBlocked { .. } => {
                return ClassifierVerdict::NoVerdict(NoVerdict::Refused);
            }
            ModelError::RateLimited { retry_after_ms, .. } => {
                (UnavailableCause::RateLimited, retry_after_ms)
            }
            ModelError::Overloaded { .. } => (UnavailableCause::Overloaded, None),
            ModelError::Server { .. } => (UnavailableCause::ServerError, None),
            ModelError::Network { .. } => (UnavailableCause::ConnectionFailed, None),
            ModelError::StreamIdle { .. } => (UnavailableCause::TimedOut, None),
            ModelError::Authentication { .. }
            | ModelError::InvalidRequest { .. }
            | ModelError::Protocol { .. }
            | ModelError::FallbackTriggered { .. }
            | ModelError::Unsupported { .. }
            | ModelError::ExecutionFellBack { .. }
            | ModelError::QuotaExhausted { .. }
            | ModelError::RequestTooLarge { .. } => (UnavailableCause::Other, None),
        },
    };
    ClassifierVerdict::NoVerdict(NoVerdict::Unavailable(Unavailable {
        model,
        cause,
        retry_after_ms,
    }))
}

/// 主循环这一步的 token 数：历史里最后一次的用量（输入、两种缓存与输出之和）。
fn main_loop_tokens(context: &ToolUseContext) -> u64 {
    let Some(transcript) = &context.transcript else {
        return 0;
    };
    transcript
        .history
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(assistant) => assistant.usage.as_ref(),
            _ => None,
        })
        .map_or(0, |usage| {
            usage.input_tokens
                + usage.cache_creation_input_tokens
                + usage.cache_read_input_tokens
                + usage.output_tokens
        })
}

/// 每次结论一条 debug（和消息流同数量级，只在本地开发打印）；模型不可用、对话太长是 info（生产保留）。不写输入与理由原文。
fn log(tool: &str, verdict: &ClassifierVerdict) {
    match verdict {
        ClassifierVerdict::NoVerdict(NoVerdict::Unavailable(unavailable)) => tracing::info!(
            event = "classifier.unavailable",
            tool,
            model = %unavailable.model,
            cause = ?unavailable.cause,
            "the auto mode classifier is unavailable"
        ),
        ClassifierVerdict::NoVerdict(NoVerdict::TranscriptTooLong) => tracing::info!(
            event = "classifier.too_long",
            tool,
            "the conversation is too long for the auto mode classifier"
        ),
        ClassifierVerdict::Allow
        | ClassifierVerdict::Block { .. }
        | ClassifierVerdict::NoVerdict(_) => {
            tracing::debug!(event = "classifier.verdict", tool, verdict = ?kind(verdict), "classified");
        }
    }
}

/// 日志里的结论种类（不带理由原文）。
fn kind(verdict: &ClassifierVerdict) -> &'static str {
    match verdict {
        ClassifierVerdict::Allow => "allow",
        ClassifierVerdict::Block { .. } => "block",
        ClassifierVerdict::NoVerdict(NoVerdict::NothingToJudge) => "nothing_to_judge",
        ClassifierVerdict::NoVerdict(NoVerdict::Unparseable) => "unparseable",
        ClassifierVerdict::NoVerdict(NoVerdict::Refused) => "refused",
        ClassifierVerdict::NoVerdict(NoVerdict::Unavailable(_)) => "unavailable",
        ClassifierVerdict::NoVerdict(NoVerdict::TranscriptTooLong) => "too_long",
        ClassifierVerdict::NoVerdict(NoVerdict::Cancelled) => "cancelled",
    }
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "request_parity_tests.rs"]
mod parity_tests;
