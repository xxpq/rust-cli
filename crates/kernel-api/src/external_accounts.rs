//! 外部账号层（docs/specs/rust-cli/platform.md §12，ACCOUNT-OVERLAY）：宿主给的账号事实——每个账号 provider 这一刻能不能用、
//! 是不是当前连接、有没有权威的模型白名单。平台层有它时不读凭据、不联网，账号 provider 以它为准；TUI 与 `-p` 不用它。
//!
//! 一个进程一个格子：前端写（同步，不等 IO），平台层读。内容变了版本加一，平台层据版本决定要不要重建目录；
//! 第一份到之前 [`ExternalAccounts::current`] 为 None（账号 provider 一律不授权）。宿主不下发时（握手里没有这项能力）前端
//! [`ExternalAccounts::decline`]：等第一份的一方不再等。
//!
//! 每一份带宿主算它时依据的内置配置摘要（[`AccountLayer::builtin_digest`]）：格子照收，用不用由平台层比过自己读到的内置配置再定。

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::watch;

/// 一个账号 provider 的事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGrant {
    /// 这一刻能不能用（宿主的最终值）。
    pub entitled: bool,
    /// 是不是当前连接；None 是宿主不定义（`false` 的 provider 不可执行）。
    pub current: Option<bool>,
    /// 这一刻权威的模型白名单；None 是用内置配置的模型。空的白名单就是没有可用的模型。
    pub models: Option<Vec<String>>,
}

/// provider id → 事实。
pub type AccountGrants = BTreeMap<String, AccountGrant>;

/// 宿主给的一份账号层：账号事实，与宿主算它时依据的内置配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLayer {
    /// 宿主算这份账号层时读的内置配置的摘要：文件字节的 sha256，64 位小写十六进制。平台层读到的内置配置的摘要与它相同才用这份。
    pub builtin_digest: String,
    pub grants: AccountGrants,
}

/// 格子里的一份：版本只在本进程里单调。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountFacts {
    pub version: u64,
    pub layer: Arc<AccountLayer>,
}

/// 格子的状态：还在等第一份、宿主不下发、或者有了一份。
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Waiting,
    Declined,
    Arrived(AccountFacts),
}

/// 进程级的格子。克隆共用同一个值。
#[derive(Clone)]
pub struct ExternalAccounts(Arc<watch::Sender<State>>);

impl Default for ExternalAccounts {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalAccounts {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(State::Waiting)))
    }

    /// 换上一份。内容（账号事实与内置配置摘要）与现有的一份相同时不变，返回假。
    pub fn set(&self, layer: AccountLayer) -> bool {
        self.0.send_if_modified(|state| {
            let version = match state {
                State::Arrived(facts) if *facts.layer == layer => return false,
                State::Arrived(facts) => facts.version + 1,
                State::Waiting | State::Declined => 1,
            };
            *state = State::Arrived(AccountFacts {
                version,
                layer: Arc::new(layer),
            });
            true
        })
    }

    /// 宿主不下发账号层（握手里没有协商出这项能力）：等第一份的一方不再等；之后照旧没有事实（账号 provider 一律不授权）。
    pub fn decline(&self) {
        self.0.send_if_modified(|state| {
            if *state != State::Waiting {
                return false;
            }
            *state = State::Declined;
            true
        });
    }

    /// 现在的一份；第一份到之前（或宿主不下发）为 None。
    #[must_use]
    pub fn current(&self) -> Option<AccountFacts> {
        match &*self.0.borrow() {
            State::Arrived(facts) => Some(facts.clone()),
            State::Waiting | State::Declined => None,
        }
    }

    /// 等第一份到，或者宿主说不下发（已经是这两种之一时立即返回）。格子在本进程里一直活着，所以等待只以这两件事结束。
    pub async fn arrived(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|state| *state != State::Waiting).await;
    }

    /// 等一份比 `seen` 新的（已经有了就立即返回），返回它的版本。读的是现值：连续几次换代只看得到最后一次。
    /// 调用方自己持有这个格子（克隆共用发送端），所以等待只以换代结束；宿主不下发时一直等（不会再变）。
    pub async fn changed_after(&self, seen: u64) -> u64 {
        let mut receiver = self.0.subscribe();
        let newer = |state: &State| matches!(state, State::Arrived(facts) if facts.version > seen);
        let _ = receiver.wait_for(newer).await;
        match &*receiver.borrow() {
            State::Arrived(facts) => facts.version,
            State::Waiting | State::Declined => seen,
        }
    }
}

#[cfg(test)]
#[path = "external_accounts_tests.rs"]
mod tests;
