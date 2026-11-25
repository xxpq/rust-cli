//! 一轮的执行覆盖（docs/specs/rust-cli/session-lifecycle.md §10.39）：这一轮用哪个模型执行、
//! 凭据向来源要时带什么引用、失败了退到哪里。前端无关：v5 前端从 `input.send.modelExecution` 换来；TUI 与 `-p` 不带。
//!
//! 只在内存：随输入排队、开回合，运行结束就丢；不落库、不进会话的模型（会话的模型只由输入的执行配置写，§10.25）。
//! 凭据不在这里：引用是一段不透明的文字，平台层取端点时原样交给来源，由来源按它现取凭据。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::model::ModelSelection;

/// 一轮的执行覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnExecution {
    /// 这一轮的模型请求用它；不写成会话的模型。
    pub selection: ModelSelection,
    /// 凭据引用（不透明）：取端点时原样放进 [`crate::provider::HeaderRequest::execution`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    /// 失败时退回会话的模型的规则；没有时执行请求照常重试、不退回。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<ExecutionFallback>,
}

/// 退回规则：请求失败时按序取第一条命中的规则（原因是给前端的事实，内核不解释）；取端点时选择不可用用 `unavailable`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionFallback {
    pub rules: Vec<FallbackRule>,
    pub unavailable: String,
}

/// 一条退回规则：带业务码的只在失败的业务码相同时命中，不带的对任何请求失败都命中。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRule {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
}

impl ExecutionFallback {
    /// 一次请求失败的退回原因：`code` 是端点给的业务码（没有时为 None）；没有命中的规则时为 None（不退回）。
    #[must_use]
    pub fn reason(&self, code: Option<&str>) -> Option<&str> {
        self.rules
            .iter()
            .find(|rule| {
                rule.provider_code
                    .as_deref()
                    .is_none_or(|wanted| Some(wanted) == code)
            })
            .map(|rule| rule.reason.as_str())
    }
}

/// 一轮的执行覆盖退回了（docs/specs/rust-cli/session-lifecycle.md §10.48）：命中的规则的原因与会话 actor 记下它时的时钟（Unix 毫秒）。
/// 回合记录（[`crate::store::TurnRecord::fell_back`]）与 [`crate::session_api::KernelEvent::ExecutionFellBack`] 带同一份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FellBack {
    pub reason: String,
    pub at: u64,
}

/// 一次运行的执行覆盖与它退回了没有（session-lifecycle.md §10.39、agents.md §4.1）。克隆共用同一个标记：主循环退回时写（唯一的写者），Agent 工具派生子代理时读，
/// 所以退回之后启动的子代理用会话的模型，之前已经在跑的不受影响。
#[derive(Debug, Clone)]
pub struct RunExecution(Arc<State>);

#[derive(Debug)]
struct State {
    execution: TurnExecution,
    fell_back: AtomicBool,
}

impl RunExecution {
    #[must_use]
    pub fn new(execution: TurnExecution) -> Self {
        Self(Arc::new(State {
            execution,
            fell_back: AtomicBool::new(false),
        }))
    }

    /// 还没退回时的执行覆盖；退回之后为 None（这次运行剩下的请求用会话的模型）。
    #[must_use]
    pub fn active(&self) -> Option<&TurnExecution> {
        (!self.0.fell_back.load(Ordering::Acquire)).then_some(&self.0.execution)
    }

    /// 退回（只退一次）：这一次调用退的返回真，已经退回过返回假。
    pub fn fall_back(&self) -> bool {
        !self.0.fell_back.swap(true, Ordering::AcqRel)
    }
}

/// 同一份执行覆盖、同样退没退回（`QueryRequest` 的比较用）。
impl PartialEq for RunExecution {
    fn eq(&self, other: &Self) -> bool {
        self.0.execution == other.0.execution && self.active().is_some() == other.active().is_some()
    }
}

#[cfg(test)]
#[path = "turn_execution_tests.rs"]
mod tests;
