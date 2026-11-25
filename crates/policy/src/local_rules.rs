//! LocalSettings 层（本机项目规则集）的读回与替换（shell-permissions.md §5、tui-manage.md §1.1）。
//!
//! 规则只有快照一个来源：“总是允许”与 `/permissions` 的修改先落库，再用落库后读回的规则集换上新的 LocalSettings 层
//! （快照是库的投影，不另存）。判决链不做 IO，库由 [`LocalRulesSource`] 读（runtime 用会话存储实现）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{
    PermissionDecision, PermissionRules, ProjectRules, RuleSource,
};

use crate::PolicyGate;

/// 本机项目规则集从哪里读回。
#[async_trait]
pub trait LocalRulesSource: Send + Sync {
    /// `workspace` 的本机项目规则集；读不出时为 None（保留旧快照）。
    async fn read(&self, workspace: &WorkspaceRef) -> Option<ProjectRules>;
}

/// 会话生效的规则集从哪里现读（云端：规则在共享库里，别的 Worker 与子代理随时可能改，deployment.md §9）。接上时每次
/// 判决前读一次；本机不接（规则只在本进程里改，快照就是库的投影）。
#[async_trait]
pub trait SessionRulesSource: Send + Sync {
    /// 会话生效的规则集（群会话是会话自己的，其余是所在项目的共享规则集）；读不出时 Err。
    async fn read_session(&self, session: &SessionId) -> Result<Option<ProjectRules>, String>;
}

impl PolicyGate {
    /// 接上会话规则的现读来源（只在云端接，见 [`SessionRulesSource`]）。
    #[must_use]
    pub fn with_session_rules(mut self, source: std::sync::Arc<dyn SessionRulesSource>) -> Self {
        self.session_source = Some(source);
        self
    }

    /// 判决前现读会话规则、换上会话层。没有来源时什么都不做；读不出时给出拒绝：不能沿用旧快照里的允许结果。
    pub(crate) async fn refresh_session(&self, session: &SessionId) -> Option<PermissionDecision> {
        let source = self.session_source.as_ref()?;
        match source.read_session(session).await {
            Ok(Some(rules)) => {
                self.scope(session, &rules);
                None
            }
            Ok(None) | Err(_) => Some(PermissionDecision::Deny {
                message: "session permission rules are unavailable".into(),
            }),
        }
    }

    /// “总是允许”写进会话的规则集之后读回：有现读来源时按会话读，否则读回本机项目规则集。
    pub(crate) async fn reload_session(&self, session: &SessionId, workspace: &WorkspaceRef) {
        let Some(source) = &self.session_source else {
            return self.reload_local(workspace).await;
        };
        match source.read_session(session).await {
            Ok(Some(rules)) => self.scope(session, &rules),
            Ok(None) => self.reload_local(workspace).await,
            // 读不出时保留旧快照：下一次判决前会再读，仍读不出就拒绝（`refresh_session`）。
            Err(_) => {}
        }
    }

    /// 接上本机项目规则集的来源（[`PermissionGate::local_rules_committed`] 从它读回）。
    ///
    /// [`PermissionGate::local_rules_committed`]: rust_cli_kernel_api::orchestration::PermissionGate::local_rules_committed
    #[must_use]
    pub fn with_local_rules(mut self, source: std::sync::Arc<dyn LocalRulesSource>) -> Self {
        self.local = Some(source);
        self
    }

    /// 换上新快照：其余来源不变，LocalSettings 换成 `local`（落库后读回的规则集）。
    pub fn replace_local(&self, local: &ProjectRules) {
        let snapshot = self.rules();
        let keep = |sources: &[RuleSource]| {
            snapshot
                .rules
                .iter()
                .filter(|rule| sources.contains(&rule.source))
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut rules = keep(&[RuleSource::UserSettings, RuleSource::ProjectSettings]);
        rules.extend(local.sourced());
        // 快照里没有会话层（它在会话 actor 里，随调用上下文来，permission-updates.md §3）。
        rules.extend(keep(&[RuleSource::CliArg]));
        self.replace_rules(PermissionRules {
            version: snapshot.version + 1,
            rules,
            ..(*snapshot).clone()
        });
    }

    /// 从来源读回 `workspace` 的规则集、换上 LocalSettings 层；没有来源或读不出时保留旧快照。
    pub(crate) async fn reload_local(&self, workspace: &WorkspaceRef) {
        let Some(source) = &self.local else {
            return;
        };
        if let Some(rules) = source.read(workspace).await {
            self.replace_local(&rules);
        }
    }
}
