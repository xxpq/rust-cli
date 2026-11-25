//! 一组权限更新的纯函数（permission-updates.md §2、§3）。

use super::*;

fn rule(tool: &str, content: Option<&str>) -> PermissionRule {
    PermissionRule {
        tool: tool.to_owned(),
        content: content.map(str::to_owned),
    }
}

fn session(behavior: RuleBehavior, rule: PermissionRule) -> SourcedRule {
    SourcedRule {
        source: RuleSource::Session,
        behavior,
        rule,
    }
}

#[test]
fn always_allow_is_one_local_allow_update_or_nothing() {
    assert!(PermissionUpdate::always_allow(Vec::new()).is_empty());
    let updates = PermissionUpdate::always_allow(vec![rule("Bash", Some("ls *"))]);
    assert_eq!(
        updates,
        [PermissionUpdate::AddRules {
            place: UpdatePlace::Local,
            behavior: RuleBehavior::Allow,
            rules: vec![rule("Bash", Some("ls *"))],
        }]
    );
    assert!(updates[0].changes_local_rules());
}

#[test]
fn only_local_rule_updates_need_the_local_layer_read_back() {
    let session_rules = PermissionUpdate::AddRules {
        place: UpdatePlace::Session,
        behavior: RuleBehavior::Deny,
        rules: vec![rule("Write", None)],
    };
    let local_mode = PermissionUpdate::SetMode {
        place: UpdatePlace::Local,
        mode: PermissionMode::AcceptEdits,
    };
    let local_dirs = PermissionUpdate::AddDirectories {
        place: UpdatePlace::Local,
        directories: vec![EnvPath::new("/Users/dev/extra")],
    };
    let local_remove = PermissionUpdate::RemoveRules {
        place: UpdatePlace::Local,
        behavior: RuleBehavior::Ask,
        rules: vec![rule("Bash", None)],
    };
    assert!(!session_rules.changes_local_rules());
    assert!(!local_mode.changes_local_rules());
    assert!(!local_dirs.changes_local_rules());
    assert!(local_remove.changes_local_rules());
}

/// 会话层：加去重，换掉只动那一类，删按规则；`Local` 的与非规则的更新不动它。
#[test]
fn the_session_layer_adds_replaces_and_removes_one_kind() {
    let mut layer = Vec::new();
    let add = |behavior, rules: Vec<PermissionRule>| PermissionUpdate::AddRules {
        place: UpdatePlace::Session,
        behavior,
        rules,
    };
    apply_to_layer(
        &mut layer,
        &add(
            RuleBehavior::Allow,
            vec![
                rule("Read", None),
                rule("Bash", Some("ls *")),
                rule("Read", None),
            ],
        ),
    );
    apply_to_layer(
        &mut layer,
        &add(RuleBehavior::Deny, vec![rule("Write", None)]),
    );
    assert_eq!(
        layer,
        [
            session(RuleBehavior::Allow, rule("Read", None)),
            session(RuleBehavior::Allow, rule("Bash", Some("ls *"))),
            session(RuleBehavior::Deny, rule("Write", None)),
        ]
    );
    apply_to_layer(
        &mut layer,
        &PermissionUpdate::ReplaceRules {
            place: UpdatePlace::Session,
            behavior: RuleBehavior::Allow,
            rules: vec![rule("Glob", None)],
        },
    );
    assert_eq!(
        layer,
        [
            session(RuleBehavior::Deny, rule("Write", None)),
            session(RuleBehavior::Allow, rule("Glob", None)),
        ]
    );
    apply_to_layer(
        &mut layer,
        &PermissionUpdate::RemoveRules {
            place: UpdatePlace::Session,
            behavior: RuleBehavior::Allow,
            rules: vec![rule("Write", None), rule("Glob", None)],
        },
    );
    assert_eq!(layer, [session(RuleBehavior::Deny, rule("Write", None))]);
    let untouched = layer.clone();
    for update in [
        PermissionUpdate::AddRules {
            place: UpdatePlace::Local,
            behavior: RuleBehavior::Allow,
            rules: vec![rule("Read", None)],
        },
        PermissionUpdate::SetMode {
            place: UpdatePlace::Session,
            mode: PermissionMode::AcceptEdits,
        },
        PermissionUpdate::AddDirectories {
            place: UpdatePlace::Session,
            directories: vec![EnvPath::new("/Users/dev/extra")],
        },
    ] {
        apply_to_layer(&mut layer, &update);
    }
    assert_eq!(layer, untouched);
}

/// 落在本机的规则与目录按顺序换成事实；`Session` 的与模式不在这里；相对路径的目录先被 `absolute_only` 去掉。
#[test]
fn local_updates_become_facts_in_order() {
    let root = EnvPath::new("/Users/dev/repo");
    let updates = [
        PermissionUpdate::AddRules {
            place: UpdatePlace::Session,
            behavior: RuleBehavior::Allow,
            rules: vec![rule("Read", None)],
        },
        PermissionUpdate::ReplaceRules {
            place: UpdatePlace::Local,
            behavior: RuleBehavior::Deny,
            rules: vec![rule("Write", None)],
        },
        PermissionUpdate::SetMode {
            place: UpdatePlace::Local,
            mode: PermissionMode::AcceptEdits,
        },
        PermissionUpdate::AddDirectories {
            place: UpdatePlace::Local,
            directories: vec![EnvPath::new("/Users/dev/extra"), EnvPath::new("relative")],
        },
        PermissionUpdate::RemoveRules {
            place: UpdatePlace::Local,
            behavior: RuleBehavior::Ask,
            rules: vec![rule("Bash", None)],
        },
        PermissionUpdate::RemoveDirectories {
            place: UpdatePlace::Local,
            directories: vec![EnvPath::new("/Users/dev/old")],
        },
    ];
    assert_eq!(
        local_facts(&absolute_only(&updates, &root)),
        [
            Fact::PermissionRulesReplaced {
                behavior: RuleBehavior::Deny,
                rules: vec![rule("Write", None)],
            },
            Fact::DirectoriesRemembered {
                directories: vec![EnvPath::new("/Users/dev/extra")],
            },
            Fact::PermissionRulesRemoved {
                behavior: RuleBehavior::Ask,
                rules: vec![rule("Bash", None)],
            },
            Fact::DirectoriesForgotten {
                directories: vec![EnvPath::new("/Users/dev/old")],
            },
        ]
    );
}
