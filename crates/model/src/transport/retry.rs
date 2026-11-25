//! 一次失败之后：按处置表的动作决定等多久再发、走 G，还是把错误交给调用方（docs/specs/rust-cli/provider-errors.md §2.4、
//! §3.2、§4，修复线 F113）。响应头之前（P0）与交出边界之前（P1）共用这一处；计数都在 [`Connect`]，每个只由这里改。

use std::time::Duration;

use rust_cli_kernel_api::model::{ModelError, ModelNotice};
use rust_cli_kernel_api::provider::{AccessDenial, EndpointQuery};
use rust_cli_net::retry::jitter;

use super::{
    Connect, MAX_OVERLOADED_BEFORE_FALLBACK, REPEATED_OVERLOAD, millis, stops_on_repeated_overload,
};
use crate::errors::dispose::{Action, Exhausted, Phase, dispose};
use crate::errors::{self, Class, Verdict};
use crate::trace::tape::{End, Tape, end};

/// 账号套餐的准入繁忙之后第 n 次重发前固定等多久（provider-errors.md §5.3.2：1 s、2 s，最多 2 次，不退避、不看 Retry-After）。
const ADMISSION_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];

/// 交出边界之前的截断最多重试这么多次，之后走 G（provider-errors.md D17 暂定）。
const TRUNCATION_RETRIES: u32 = 1;

/// 空回复最多重试这么多次（provider-errors.md §3.7，Node 的 `EMPTY_COMPLETION_MAX_RETRIES`）。
const EMPTY_REPLY_RETRIES: u32 = 1;

impl Connect {
    /// 一次失败：要重发时返回重试通知与等待时长（这次发送的模型轨迹记 `Failed` 带等待）；否则返回给调用方的错误。
    /// 凭据被拒、端点可续期时先续期（只换凭据与 Provider header；请求体已按原端点编码，不再改）。
    pub(super) async fn fail(
        mut self: Box<Self>,
        verdict: Verdict,
        phase: Phase,
        tape: Option<Tape>,
    ) -> Result<(ModelNotice, Box<Self>, Duration), ModelError> {
        // 修 bug（照 Node，provider-errors.md §3.7）：空回复以前一律按空回复重试。带着一次性校验的请求，厂商可能用空回复表示
        // 校验无效（智谱的 3007）：同一个用过的校验再发一次还是空。空回复先交厂商层重读，认出时走下面的「问来源」。
        let verdict = if verdict.class == Class::EmptyReply {
            errors::empty_reply(
                self.vendor,
                self.endpoint.protocol,
                self.endpoint.single_use,
                verdict,
            )
        } else {
            verdict
        };
        // 单次尝试（model.md §5.2）：带退回规则的请求首次失败就按规则判，不重试、不续期、不问来源
        if self.rules.is_some() {
            end(tape, End::Failed(verdict.error.clone(), None));
            return Err(super::execution::fell_back(
                self.rules.as_ref(),
                super::Stage::Request(verdict.code.clone()),
                verdict.error,
            ));
        }
        if verdict.class == Class::AccessDenied && self.endpoint.renewable && !self.asked {
            return self.ask_source(verdict, tape).await;
        }
        let delay = match self.delay_for(&verdict, phase) {
            Ok(delay) => {
                end(
                    tape,
                    End::Failed(verdict.error.clone(), Some(millis(delay))),
                );
                delay
            }
            Err(error) => {
                end(tape, End::Failed(verdict.error, None));
                return Err(error);
            }
        };
        if matches!(verdict.class, Class::AuthRejected | Class::LoginExpired)
            && self.endpoint.renewable
        {
            // 每请求取的凭据被拒或登录失效：带上被拒的端点再取一次（刷新凭据再重试）。
            let renewed = self.route.endpoints.endpoint(EndpointQuery {
                selection: &self.selection,
                rejected: Some(&self.endpoint),
                output_limit: self.output_limit,
                session: self.session.as_ref(),
                root: self.root.as_ref(),
                denied: None,
                execution: self.execution.as_deref(),
            });
            match self.cancel.run_until_cancelled(renewed).await {
                None => return Err(ModelError::Cancelled),
                Some(Err(error)) => return Err(error),
                Some(Ok(endpoint)) => self.renew(endpoint),
            }
        }
        let delay_ms = millis(delay);
        let max_retries = self.max_retries;
        // 带会话的 `model.retry`（warn）由 query 从重试通知记；这里是所有调用方的细节（logging.md §4）。
        tracing::debug!(
            event = "model.attempt.failed",
            model = %self.selection.model,
            attempt = self.attempt,
            max_retries,
            delay_ms,
            phase = ?phase,
            class = ?verdict.class,
            error = %verdict.error,
            "model request failed; retrying"
        );
        let notice = ModelNotice::Retrying {
            attempt: self.attempt,
            max_retries,
            delay_ms,
            error: verdict.error,
        };
        self.attempt += 1;
        self.notified = true;
        Ok((notice, self, delay))
    }

    /// 拒绝访问（不是凭据被拒）、凭据每请求取：带上拒绝的事实问来源一次（model.md §5.2）。来源给了新的凭据 → 换上、立即重发（不占
    /// 普通的重试次数，同 Node 的一次性补发）；给不了 → 交出原来的错误：拒绝的原因才是给用户的事实，来源的文字只说明它帮不上。
    /// 处置表里拒绝访问不重试，所以以前这里直接交出错误。
    async fn ask_source(
        mut self: Box<Self>,
        verdict: Verdict,
        tape: Option<Tape>,
    ) -> Result<(ModelNotice, Box<Self>, Duration), ModelError> {
        self.asked = true;
        let denied = AccessDenial {
            status: verdict.status,
            code: verdict.code.clone(),
        };
        let renewed = self.route.endpoints.endpoint(EndpointQuery {
            selection: &self.selection,
            rejected: None,
            output_limit: self.output_limit,
            session: self.session.as_ref(),
            root: self.root.as_ref(),
            denied: Some(&denied),
            execution: self.execution.as_deref(),
        });
        let endpoint = match self.cancel.run_until_cancelled(renewed).await {
            None => {
                end(tape, End::Failed(verdict.error.clone(), None));
                return Err(ModelError::Cancelled);
            }
            Some(Err(_)) => {
                end(tape, End::Failed(verdict.error.clone(), None));
                return Err(verdict.error);
            }
            Some(Ok(endpoint)) => endpoint,
        };
        end(tape, End::Failed(verdict.error.clone(), Some(0)));
        self.renew(endpoint);
        let notice = ModelNotice::Retrying {
            attempt: self.attempt,
            max_retries: self.max_retries,
            delay_ms: 0,
            error: verdict.error,
        };
        self.notified = true;
        Ok((notice, self, Duration::ZERO))
    }

    /// 处置表的动作换成等待时长；不再重发时是给调用方的错误（换 fallback、连续过载、不可重试或到了上限）。
    fn delay_for(&mut self, verdict: &Verdict, phase: Phase) -> Result<Duration, ModelError> {
        let failed = || verdict.error.clone();
        let action = dispose(
            verdict.class,
            phase,
            self.overload_retry,
            verdict.should_retry,
        );
        let delay = match action {
            Action::Fail => None,
            Action::Resend => self.resend(),
            Action::Retry { exhausted } => self.backoff(verdict).or_else(|| match exhausted {
                Exhausted::Resend => self.resend(),
                Exhausted::Fail => None,
            }),
            // 修改（主会话审定 2026-11-06，照 Node）：F113 对所有请求重试 2 次（通常的退避）；现在只有 Start Plan 主回合的准入
            // 固定等 1 s、2 s 重发，不受重试次数的上限限制（那一处的重试不在请求的重试预算里），别的请求不重试。
            Action::Capacity => {
                let delay = usize::try_from(self.capacity)
                    .ok()
                    .and_then(|index| ADMISSION_DELAYS.get(index))
                    .filter(|_| self.admission_retry)
                    .copied();
                self.capacity += 1;
                delay
            }
            // 修 bug（主会话审定 2026-11-06）：登录失效（1006）以前按凭据被拒处理，不能续期的端点上退避重试到上限，白等。只有
            // 换凭据才有用：能续期时续期一次再发（`fail` 里换凭据），否则不重试。
            Action::Renew if self.endpoint.renewable && !self.renewed => {
                self.renewed = true;
                self.backoff(verdict)
            }
            Action::Renew => None,
            // 修 bug（PROVIDER-RETRY，Q20）：空回复以前按正常完成交出，回合静默结束、没有正文。照 Node 作废、退避后重试一次；
            // 次数在总次数之内（`backoff`），再空时把空回复的错误交给调用方。
            Action::Empty => {
                self.empties += 1;
                if self.empties > EMPTY_REPLY_RETRIES {
                    None
                } else {
                    self.backoff(verdict)
                }
            }
            Action::Truncation => {
                self.truncations += 1;
                if self.truncations > TRUNCATION_RETRIES {
                    self.resend()
                } else {
                    self.backoff(verdict).or_else(|| self.resend())
                }
            }
            Action::Overload => {
                self.overloaded += 1;
                if self.overloaded >= MAX_OVERLOADED_BEFORE_FALLBACK {
                    if let Some(fallback) = self.fallback.take() {
                        return Err(ModelError::FallbackTriggered {
                            original_model: self.selection.model.clone(),
                            fallback_model: fallback,
                        });
                    }
                    match phase {
                        // 修 bug（F10）：以前没有 fallback 时一律重试到上限（11 次请求、约 2.5 分钟）；应对按系列不再重试的模型
                        // （`stops_on_repeated_overload`）在第 3 次过载后报 `Repeated 529 Overloaded errors`。
                        Phase::Head
                            if !self.route.options.sandbox
                                && stops_on_repeated_overload(&self.selection.model) =>
                        {
                            return Err(ModelError::Overloaded {
                                message: REPEATED_OVERLOAD.to_owned(),
                            });
                        }
                        Phase::Head => {}
                        // 流内第 3 次过载、没有 fallback：走 G（非流式重发），不看模型系列。
                        Phase::Stream => return self.resend().ok_or_else(failed),
                    }
                }
                match phase {
                    Phase::Head => self.backoff(verdict),
                    Phase::Stream => self.backoff(verdict).or_else(|| self.resend()),
                }
            }
        };
        delay.ok_or_else(failed)
    }

    /// 退避等待（认 Retry-After；等待超过上限时不等）；次数用尽时为 None。
    fn backoff(&self, verdict: &Verdict) -> Option<Duration> {
        let retry = &self.route.options.retry;
        if self.attempt > self.max_retries {
            return None;
        }
        retry.delay(self.attempt, verdict.retry_after, jitter())
    }

    /// G：请求换成非流式的，不等待、第一次不受次数上限限制；之后这次调用里的请求都是它（provider-errors.md §3.6）。已经是
    /// 非流式、入口关了它、或协议改写不了请求时为 None。
    ///
    /// 修改（PROVIDER-RETRY，用户决定 Q22）：F113 用一次流式重发代替（D3 暂定 (b)），同样的流式请求在同样的端点上多半同样失败
    /// （例如压缩摘要请求遇到不支持流式的端点）；现在改用非流式请求。
    pub(super) fn resend(&mut self) -> Option<Duration> {
        if self.whole || self.route.options.disable_nonstreaming_fallback {
            return None;
        }
        self.encoded = self.protocol.unstreamed(&self.encoded)?;
        self.whole = true;
        Some(Duration::ZERO)
    }
}
