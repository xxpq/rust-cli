//! 一组权限更新（docs/specs/rust-cli/permission-updates.md §2）：一次「允许」一并套用的改动。用户的「总是允许」与
//! PermissionRequest hook 的 `updatedPermissions` 都换成它；只有会话 actor 落它（§4：先算出事实，一批提交，成功后改内存）。

use serde::{Deserialize, Serialize};

use crate::env::EnvPath;
use crate::env_files::PathStyle;
use crate::permission::{PermissionMode, PermissionRule, RuleBehavior, RuleSource, SourcedRule};
use crate::store::Fact;

/// 一项更新落在哪里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePlace {
    /// 只在这个会话（会话 actor 的内存，会话关闭就没有）。
    Session,
    /// 这台机器的这个项目（Node 的 `local_setting`：本机项目规则集与持久模式），并在这个会话里立即生效。
    Local,
}

/// 一项权限更新（hook 的六种类型，destination 换成 [`UpdatePlace`]，permission-updates.md §2.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionUpdate {
    /// 加规则（按规则去重）。
    AddRules {
        place: UpdatePlace,
        behavior: RuleBehavior,
        rules: Vec<PermissionRule>,
    },
    /// 这一处这一类的规则整份换成 `rules`。
    ReplaceRules {
        place: UpdatePlace,
        behavior: RuleBehavior,
        rules: Vec<PermissionRule>,
    },
    /// 去掉这一处这一类里的这些规则。
    RemoveRules {
        place: UpdatePlace,
        behavior: RuleBehavior,
        rules: Vec<PermissionRule>,
    },
    /// 切会话的权限模式（`Plan` 是打开 plan 开关）；`Local` 另记为项目的持久模式。
    SetMode {
        place: UpdatePlace,
        mode: PermissionMode,
    },
    /// 加附加工作目录（绝对路径；已在工作目录里的不加）；`Local` 另记进本机项目规则集。
    AddDirectories {
        place: UpdatePlace,
        directories: Vec<EnvPath>,
    },
    /// 去掉附加工作目录；`Local` 另从本机项目规则集里去掉。
    RemoveDirectories {
        place: UpdatePlace,
        directories: Vec<EnvPath>,
    },
}

impl PermissionUpdate {
    /// 用户的「总是允许」（询问里选「不再询问」）：这些允许规则加进本机项目规则集；没有规则时没有更新。
    pub fn always_allow(rules: Vec<PermissionRule>) -> Vec<Self> {
        if rules.is_empty() {
            return Vec::new();
        }
        vec![Self::AddRules {
            place: UpdatePlace::Local,
            behavior: RuleBehavior::Allow,
            rules,
        }]
    }

    /// 改本机项目规则集里的规则：落库之后判决链要读回 LocalSettings 层（`PermissionGate::local_rules_committed`）。
    pub fn changes_local_rules(&self) -> bool {
        matches!(
            self,
            Self::AddRules {
                place: UpdatePlace::Local,
                ..
            } | Self::ReplaceRules {
                place: UpdatePlace::Local,
                ..
            } | Self::RemoveRules {
                place: UpdatePlace::Local,
                ..
            }
        )
    }
}

/// 去掉 `AddDirectories` 里的相对路径（按工作区根 `root` 推断路径风格，同 `working_dirs::admit`：相对路径的基准说不清）。
/// 落更新的一方（会话 actor、`-p` 里后台子代理的输出端）先过这一步，内存与事实用同一份。
pub fn absolute_only(updates: &[PermissionUpdate], root: &EnvPath) -> Vec<PermissionUpdate> {
    let style = PathStyle::infer(root.as_str());
    updates
        .iter()
        .map(|update| match update {
            PermissionUpdate::AddDirectories { place, directories } => {
                PermissionUpdate::AddDirectories {
                    place: *place,
                    directories: directories
                        .iter()
                        .filter(|dir| style.is_absolute(dir.as_str()))
                        .cloned()
                        .collect(),
                }
            }
            other => other.clone(),
        })
        .collect()
}

/// 一组更新里落在本机的规则与目录要提交的事实，按更新的顺序（permission-updates.md §2.3）：规则的加、换、删，目录的记住与去掉。
/// 模式（`ExecutionChanged`、`ProjectModeSet`）不在这里：它要与会话的内存一起改，由会话 actor 自己加。
pub fn local_facts(updates: &[PermissionUpdate]) -> Vec<Fact> {
    updates
        .iter()
        .filter_map(|update| match update {
            PermissionUpdate::AddRules {
                place: UpdatePlace::Local,
                behavior,
                rules,
            } => Some(Fact::PermissionRulesAdded {
                behavior: *behavior,
                rules: rules.clone(),
            }),
            PermissionUpdate::ReplaceRules {
                place: UpdatePlace::Local,
                behavior,
                rules,
            } => Some(Fact::PermissionRulesReplaced {
                behavior: *behavior,
                rules: rules.clone(),
            }),
            PermissionUpdate::RemoveRules {
                place: UpdatePlace::Local,
                behavior,
                rules,
            } => Some(Fact::PermissionRulesRemoved {
                behavior: *behavior,
                rules: rules.clone(),
            }),
            PermissionUpdate::AddDirectories {
                place: UpdatePlace::Local,
                directories,
            } => Some(Fact::DirectoriesRemembered {
                directories: directories.clone(),
            }),
            PermissionUpdate::RemoveDirectories {
                place: UpdatePlace::Local,
                directories,
            } => Some(Fact::DirectoriesForgotten {
                directories: directories.clone(),
            }),
            PermissionUpdate::AddRules { .. }
            | PermissionUpdate::ReplaceRules { .. }
            | PermissionUpdate::RemoveRules { .. }
            | PermissionUpdate::SetMode { .. }
            | PermissionUpdate::AddDirectories { .. }
            | PermissionUpdate::RemoveDirectories { .. } => None,
        })
        .collect()
}

/// 会话级规则层（permission-updates.md §3）上套用一项 `Session` 的规则更新：加（去重）、整份换掉一类、删；别的更新不动它。
/// 层里的规则来源都是 [`RuleSource::Session`]。会话 actor 是唯一的调用方（owner），这里只是纯的改法。
pub fn apply_to_layer(layer: &mut Vec<SourcedRule>, update: &PermissionUpdate) {
    fn add(layer: &mut Vec<SourcedRule>, behavior: RuleBehavior, rules: &[PermissionRule]) {
        for rule in rules {
            let rule = SourcedRule {
                source: RuleSource::Session,
                behavior,
                rule: rule.clone(),
            };
            if !layer.contains(&rule) {
                layer.push(rule);
            }
        }
    }
    match update {
        PermissionUpdate::AddRules {
            place: UpdatePlace::Session,
            behavior,
            rules,
        } => add(layer, *behavior, rules),
        PermissionUpdate::ReplaceRules {
            place: UpdatePlace::Session,
            behavior,
            rules,
        } => {
            layer.retain(|known| known.behavior != *behavior);
            add(layer, *behavior, rules);
        }
        PermissionUpdate::RemoveRules {
            place: UpdatePlace::Session,
            behavior,
            rules,
        } => layer.retain(|known| known.behavior != *behavior || !rules.contains(&known.rule)),
        PermissionUpdate::AddRules { .. }
        | PermissionUpdate::ReplaceRules { .. }
        | PermissionUpdate::RemoveRules { .. }
        | PermissionUpdate::SetMode { .. }
        | PermissionUpdate::AddDirectories { .. }
        | PermissionUpdate::RemoveDirectories { .. } => {}
    }
}

#[cfg(test)]
#[path = "permission_updates_tests.rs"]
mod tests;
