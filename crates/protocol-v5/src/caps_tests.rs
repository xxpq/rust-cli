use super::*;

fn decl(items: &[(&str, &[u64])]) -> Declaration {
    items
        .iter()
        .map(|(cap, revs)| ((*cap).to_owned(), revs.to_vec()))
        .collect()
}

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|name| (*name).to_owned()).collect()
}

/// 与 packages/protocol/test/capabilities.test.ts 的偏斜示例同一组输入。
fn desktop_now() -> Declaration {
    decl(&[
        ("core", &[1]),
        ("directory", &[1]),
        ("tasks", &[1]),
        ("session", &[2, 3]),
        ("terminal", &[1]),
        ("fs", &[1]),
        ("presence", &[1]),
    ])
}

const BASE_REQUIRED: &[&str] = &["core", "directory", "tasks", "session"];

#[test]
fn the_highest_common_revision_wins_and_unknown_caps_are_absent() {
    let caps = negotiate(
        &desktop_now(),
        &decl(&[
            ("core", &[1]),
            ("directory", &[1]),
            ("tasks", &[1]),
            ("session", &[1, 2]),
            ("terminal", &[1]),
        ]),
        &names(BASE_REQUIRED),
    )
    .unwrap();
    assert_eq!(caps.get("session"), Some(&2));
    assert_eq!(caps.get("terminal"), Some(&1));
    assert!(!caps.contains_key("fs"));
}

#[test]
fn an_old_client_gets_client_and_an_old_server_gets_server() {
    let (missing, upgrade) = negotiate(
        &desktop_now(),
        &decl(&[
            ("core", &[1]),
            ("directory", &[1]),
            ("tasks", &[1]),
            ("session", &[1]),
        ]),
        &names(BASE_REQUIRED),
    )
    .unwrap_err();
    assert_eq!(missing[0].cap, "session");
    assert_eq!(upgrade, InitializeRejectionUpgradeTarget::Client);
    let (_, upgrade) = negotiate(
        &desktop_now(),
        &decl(&[
            ("core", &[1]),
            ("directory", &[1]),
            ("tasks", &[1]),
            ("session", &[4]),
        ]),
        &names(BASE_REQUIRED),
    )
    .unwrap_err();
    assert_eq!(upgrade, InitializeRejectionUpgradeTarget::Server);
    // 服务端完全没有这个能力：升级服务端（不是客户端）
    let (missing, upgrade) = negotiate(
        &decl(&[("core", &[1])]),
        &decl(&[("core", &[1]), ("workflow", &[1])]),
        &names(&["workflow"]),
    )
    .unwrap_err();
    assert_eq!(missing[0].mine, Vec::<u64>::new());
    assert_eq!(upgrade, InitializeRejectionUpgradeTarget::Server);
    let both = [
        Missing {
            cap: "a".into(),
            mine: vec![],
            theirs: vec![1],
        },
        Missing {
            cap: "b".into(),
            mine: vec![3],
            theirs: vec![1],
        },
    ];
    assert_eq!(
        upgrade_target(&both),
        InitializeRejectionUpgradeTarget::Both
    );
}

#[test]
fn the_served_caps_come_from_the_generated_registry() {
    let served = served();
    assert_eq!(
        served.keys().collect::<Vec<_>>(),
        [
            "account.overlay",
            "agent",
            "attachments",
            "catalog.plugins.reference",
            "catalog.slash",
            "core",
            "core.patch",
            "hooks.pretrust",
            "mcp.trust",
            "session",
            "session.deletion",
            "session.execution",
            "session.fork",
            "session.queue",
            "session.rewind"
        ]
    );
    assert!(served.values().all(|revs| !revs.is_empty()));
}
