//! 主循环里的压缩（docs/specs/rust-cli/compact.md §1、§4.1）：每一步发请求前的自动压缩，
//! 模型返回上下文超限后的反应式压缩。判定与压缩都在压缩器（[`Compactor`]）里，这里只负责调用、
//! 把边界、摘要与附件在一个事务里提交，并放进本次运行的历史。

use async_trait::async_trait;
use rust_cli_kernel_api::compaction::{CompactMeter, CompactProgress, prompt_too_long_gap};
use rust_cli_kernel_api::message::{CompactTrigger, Message};
use rust_cli_kernel_api::metering::{Metered, ModelCall};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor, QueryError,
    QueryStop,
};

use crate::RunScope;
use crate::recovery::quiet;

/// 压缩器的进度与摘要请求的计量转给本次运行的输出端（compact.md §4.1；usage.md §2.1.2）。
struct Progress<'a>(&'a RunScope);

#[async_trait]
impl CompactMeter for Progress<'_> {
    /// 同主循环没有消息的请求：经输出端单独一批提交，锚点是当前轮（会话 actor 或子代理的 sink 给）。写不进去只记日志，压缩照常
    /// （Node 同：观测失败不改变业务语义；标题的计量同一做法）。
    async fn metered(&self, call: ModelCall) {
        if let Err(reason) = self.0.sink.commit_metered(None, Metered::Model(call)).await {
            tracing::warn!(%reason, "the compaction usage was not stored");
        }
    }
}

#[async_trait]
impl CompactProgress for Progress<'_> {
    async fn started(&self, trigger: CompactTrigger) {
        self.0.sink.compact_started(trigger).await;
    }

    async fn failed(&self, trigger: CompactTrigger, message: String) {
        self.0.sink.compact_failed(trigger, message).await;
    }
}

/// 一次运行内的压缩状态（自动压缩的跟踪与是否试过反应式压缩）。
#[derive(Debug, Default)]
pub(crate) struct Compaction {
    tracking: CompactTracking,
    reactive_tried: bool,
}

/// 压缩请求。会话钉住的输入取这一刻的视图（压缩完才重钉，摘要请求的 system 与主线程逐字相同，context.md §15.3）。
async fn request(
    scope: &RunScope,
    history: &[Message],
    selection: &ModelSelection,
    trigger: CompactTrigger,
) -> CompactRequest {
    CompactRequest {
        session: scope.session.clone(),
        workspace: scope.workspace.clone(),
        trigger,
        selection: selection.clone(),
        instructions: None,
        messages: history.to_vec(),
        mode: Some(scope.mode),
        agent: scope.agent.clone(),
        // 修 bug（21-request-auth.md §14）：以前子代理里的压缩不带顶层会话，摘要请求与压缩的 hook 经宿主取凭据时找不到会话
        root: scope.nested_root(),
        initial_token_gap: None,
        additional_directories: scope.additional_directories.clone(),
        preferences: scope.preferences,
        prompt: scope.view().await.prompt,
    }
}

/// 边界、摘要与附件在一个事务里提交，再放进本次运行的历史。
async fn commit(
    scope: &RunScope,
    history: &mut Vec<Message>,
    result: CompactResult,
) -> Result<(), QueryError> {
    let CompactResult {
        boundary,
        summary,
        restored,
    } = result;
    let mut messages = vec![
        Message::System(rust_cli_kernel_api::message::SystemMessage::CompactBoundary(boundary)),
        Message::User(summary),
    ];
    messages.extend(restored);
    scope.sink.commit_messages(messages.clone()).await?;
    history.extend(messages);
    Ok(())
}

impl Compaction {
    /// 进入下一个工具轮。
    pub(crate) fn next_turn(&mut self) {
        self.tracking.next_turn();
        self.reactive_tried = false;
    }

    /// 发请求前的自动压缩与阻塞线；rapid refill 熔断、到了阻塞线时返回结束原因。
    pub(crate) async fn before_request(
        &mut self,
        scope: &RunScope,
        history: &mut Vec<Message>,
        selection: &ModelSelection,
        source: &QuerySource,
    ) -> Result<Option<QueryStop>, QueryError> {
        let compactor: &dyn Compactor = scope.parts.compactor.as_ref();
        let mut failure = None;
        // 先用便宜的同步判定把关：每一步都构造压缩请求要复制整个历史（README §8）。
        if !quiet(source) && compactor.should_auto_compact(history, selection) {
            let outcome = compactor
                .auto_compact_with(
                    request(scope, history, selection, CompactTrigger::Auto).await,
                    &mut self.tracking,
                    scope.abort.token().clone(),
                    &Progress(scope),
                )
                .await
                .map_err(|error| QueryError::Unsupported {
                    what: error.to_string(),
                })?;
            match outcome {
                AutoCompactOutcome::NotNeeded => {}
                // 失败已计入熔断次数，再看阻塞线（错误文字带上失败的原因）。
                AutoCompactOutcome::Failed { message } => failure = Some(message),
                AutoCompactOutcome::Thrashing { message } => {
                    return Ok(Some(QueryStop::ApiError { message }));
                }
                // 刚压缩过：这一步不看阻塞线。
                // 开始事件已由压缩器在摘要请求之前说过（compact.md §4.1）。
                //
                // 缺陷修复：以前这里在摘要完成之后才发开始事件，摘要请求期间前端只看到 `Working`。
                AutoCompactOutcome::Compacted { result } => {
                    commit(scope, history, *result).await?;
                    return Ok(None);
                }
            }
        }
        // 阻塞线（compact.md §2.2）：到了就不发请求，回合以错误结束；摘要请求不看。
        if *source != QuerySource::Compact
            && let Some(message) = compactor.blocking_limit(history, selection, failure.as_deref())
        {
            return Ok(Some(QueryStop::ApiError { message }));
        }
        Ok(None)
    }

    /// 上下文超限后的反应式压缩；压缩成功返回真，主循环重发请求。
    /// `error` 是 `ModelError::PromptTooLong` 的文本。
    pub(crate) async fn after_prompt_too_long(
        &mut self,
        scope: &RunScope,
        history: &mut Vec<Message>,
        selection: &ModelSelection,
        source: &QuerySource,
        error: &str,
    ) -> Result<bool, QueryError> {
        if self.reactive_tried || quiet(source) || scope.abort.is_aborted() {
            return Ok(false);
        }
        self.reactive_tried = true;
        scope.sink.compact_started(CompactTrigger::Reactive).await;
        // 缺陷修复（L25）：原来不带触发它的错误给出的 token 差，压缩核心第一次就转全部摘要时
        // 把整段对话发出去；应从错误取差，作为压缩的初始差，据此裁掉开头。
        let mut compact = request(scope, history, selection, CompactTrigger::Reactive).await;
        compact.initial_token_gap = prompt_too_long_gap(error);
        // 摘要请求的计量经本次运行的输出端写进用量表（usage.md §2.1.2）。开始已经在上面说过，不经进度端口。
        let compacted = scope
            .parts
            .compactor
            .compact_metered(compact, scope.abort.token().clone(), &Progress(scope))
            .await;
        match compacted {
            Ok(result) => {
                commit(scope, history, result).await?;
                Ok(true)
            }
            // 压缩不了：主循环照旧以上下文超限的错误结束（放出被扣下的 413 错误）。开始事件已经发过，失败也说
            // （compact.md §4.1）；中止时回合以中止结束，不说。
            Err(error) => {
                if !scope.abort.is_aborted() {
                    scope
                        .sink
                        .compact_failed(CompactTrigger::Reactive, error.to_string())
                        .await;
                }
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
