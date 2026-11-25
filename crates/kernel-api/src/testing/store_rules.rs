//! `MemoryStore` 的本机项目规则集（node-store `project_rules` 的内存替身）：权限事实与 `remember_directories` 的同一套改法。

use crate::store::Fact;

/// 一条本机的权限事实套到规则集上（node-store `project_rules` 的语义：追加去重、替换、按规则删、目录追加去重与删）；别的事实不动。
pub(super) fn local_fact(project: &mut crate::permission::ProjectRules, fact: &Fact) {
    match fact {
        Fact::PermissionRulesAdded { behavior, rules } => push_new(list(project, *behavior), rules),
        Fact::PermissionRulesReplaced { behavior, rules } => {
            let list = list(project, *behavior);
            list.clear();
            push_new(list, rules);
        }
        Fact::PermissionRulesRemoved { behavior, rules } => {
            list(project, *behavior).retain(|rule| !rules.contains(rule));
        }
        Fact::DirectoriesRemembered { directories } => {
            push_new(&mut project.additional_directories, directories);
        }
        Fact::DirectoriesForgotten { directories } => project
            .additional_directories
            .retain(|dir| !directories.contains(dir)),
        _ => {}
    }
}

fn list(
    project: &mut crate::permission::ProjectRules,
    behavior: crate::permission::RuleBehavior,
) -> &mut Vec<crate::permission::PermissionRule> {
    use crate::permission::RuleBehavior;
    match behavior {
        RuleBehavior::Allow => &mut project.allow,
        RuleBehavior::Deny => &mut project.deny,
        RuleBehavior::Ask => &mut project.ask,
    }
}

fn push_new<T: Clone + PartialEq>(list: &mut Vec<T>, items: &[T]) {
    for item in items {
        if !list.contains(item) {
            list.push(item.clone());
        }
    }
}
