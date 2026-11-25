//! fork 出的会话从父会话继承的、不落库的起始状态（docs/specs/rust-cli/session-dirs.md §12.5）：附加工作目录；侧聊另有父会话此刻的会话层权限规则
//! （session-lifecycle.md §10.58，主会话 2026-11-10 审定照 v4）。它们都不落库，fork 出的会话又由前端另行打开（TUI 的 `/fork` 先关父会话、再开子会话），
//! 所以父会话的 actor 在 fork 写进存储之后把自己此刻的值记在这里，子会话第一次冷加载时由组装方取走。一个内核一份，交给每个会话 actor
//! （同 [`crate::DetachedAsks`]）；一直没打开的子会话的那一份留到进程退出。

use std::collections::HashMap;
use std::sync::Mutex;

use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::SourcedRule;

/// 子会话第一次冷加载时取走的起始状态。
#[derive(Debug, Default, PartialEq)]
pub struct Inherited {
    pub directories: Vec<EnvPath>,
    /// 会话层的权限规则（只有侧聊继承，fork 为空）。
    pub rules: Vec<SourcedRule>,
}

#[derive(Default)]
pub struct ForkDirectories(Mutex<HashMap<SessionId, Inherited>>);

impl ForkDirectories {
    /// 父会话的 actor：fork 写进存储之后记下附加目录（目录为空时不记）。
    pub(crate) fn inherit(&self, child: SessionId, directories: Vec<EnvPath>) {
        self.record(
            child,
            Inherited {
                directories,
                rules: Vec::new(),
            },
        );
    }

    /// 父会话的 actor：侧聊写进存储之后记下附加目录与会话层规则的快照（处理这条命令那一刻的值）。
    pub(crate) fn inherit_side_chat(&self, child: SessionId, inherited: Inherited) {
        self.record(child, inherited);
    }

    fn record(&self, child: SessionId, inherited: Inherited) {
        if inherited != Inherited::default() {
            self.0.lock_unpoisoned().insert(child, inherited);
        }
    }

    /// 组装方冷加载 `child` 成功之后取走（只取一次）；不是 fork 出的会话、或父会话什么都没有时为空。
    pub fn take(&self, child: &SessionId) -> Inherited {
        self.0.lock_unpoisoned().remove(child).unwrap_or_default()
    }
}
