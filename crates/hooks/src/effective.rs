//! 实际生效的 hooks（docs/specs/rust-cli/hooks.md §2.2）：设置快照按来源合并，再接上插件的 hooks
//! （registered hooks）。

use std::collections::HashMap;

use rust_cli_kernel_api::hook_runs::HookOrigin;
use rust_cli_kernel_api::hooks::HookEventName;

use crate::config::{HookMatcher, HookSource, HooksConfig, HooksSettings};
use crate::plugin::PluginHooks;

/// 一个 matcher 与它来自哪里（hooks.md §14.2）：合并时记下，hook 的运行事实据它给出来源。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sourced {
    pub origin: HookOrigin,
    pub matcher: HookMatcher,
}

impl HookSource {
    fn origin(self) -> HookOrigin {
        match self {
            Self::User => HookOrigin::User,
            Self::Project => HookOrigin::Project,
            Self::Local => HookOrigin::Local,
            Self::Policy => HookOrigin::Policy,
        }
    }
}

/// 合并后实际生效的配置。
#[derive(Debug, Clone, Default)]
pub(crate) struct Effective {
    pub matchers: HashMap<HookEventName, Vec<Sourced>>,
    pub allowed_urls: Option<Vec<String>>,
    pub allowed_env_vars: Option<Vec<String>>,
}

fn concat(values: impl Iterator<Item = Option<Vec<String>>>) -> Option<Vec<String>> {
    values.flatten().reduce(|mut all, next| {
        all.extend(next);
        all
    })
}

impl HooksConfig {
    /// 只有设置快照时的生效配置。
    #[cfg(test)]
    pub(crate) fn effective(&self) -> Effective {
        self.effective_with(&[])
    }

    /// 设置快照按来源的合并顺序合并，之后按插件顺序接上插件的 matcher；只用 managed hooks 时跳过插件。
    pub(crate) fn effective_with(&self, plugins: &[PluginHooks]) -> Effective {
        // 按来源的合并顺序（同一来源保持给出的顺序）。
        let mut sources: Vec<&(HookSource, HooksSettings)> = self.sources.iter().collect();
        sources.sort_by_key(|(source, _)| *source);
        let mut effective = Effective {
            allowed_urls: concat(
                sources
                    .iter()
                    .map(|(_, s)| s.allowed_http_hook_urls.clone()),
            ),
            allowed_env_vars: concat(
                sources
                    .iter()
                    .map(|(_, s)| s.http_hook_allowed_env_vars.clone()),
            ),
            ..Effective::default()
        };
        let policy = sources
            .iter()
            .filter(|(source, _)| *source == HookSource::Policy)
            .map(|(_, settings)| settings);
        if policy.clone().any(|s| s.disable_all_hooks == Some(true)) {
            return effective;
        }
        // 合并后的 disableAllHooks：后面的来源覆盖前面的。
        let merged_disable =
            sources.iter().rev().find_map(|(_, s)| s.disable_all_hooks) == Some(true);
        let managed_only = policy
            .clone()
            .any(|s| s.allow_managed_hooks_only == Some(true))
            || merged_disable;
        let allowed = |source: HookSource| !managed_only || source == HookSource::Policy;
        let plugins: &[PluginHooks] = if managed_only { &[] } else { plugins };
        // 每一层与它的来源：设置快照按来源，插件按插件名
        let layers: Vec<(HookOrigin, &HooksSettings)> = sources
            .iter()
            .filter(|(source, _)| allowed(*source))
            .map(|(source, settings)| (source.origin(), settings))
            .chain(plugins.iter().map(|plugin| {
                let origin = HookOrigin::Plugin {
                    name: plugin.context.name.clone(),
                };
                (origin, &plugin.settings)
            }))
            .collect();
        for event in HookEventName::ALL {
            let matchers: Vec<Sourced> = layers
                .iter()
                .flat_map(|(origin, settings)| {
                    settings.matchers(*event).map(|matcher| Sourced {
                        origin: origin.clone(),
                        matcher: matcher.clone(),
                    })
                })
                .collect();
            if !matchers.is_empty() {
                effective.matchers.insert(*event, matchers);
            }
        }
        effective
    }
}
