use super::*;

#[test]
fn a_user_change_reaches_every_workspace_and_a_workspace_change_its_subtree() {
    let roots = [
        "/Users/dev/app",
        "/Users/dev/app/web",
        "/Users/dev/apple",
        "/Users/dev/lib",
    ];
    let hit = |changed: PluginsChanged| -> Vec<&str> {
        roots
            .iter()
            .copied()
            .filter(|root| changed.affects(root))
            .collect()
    };
    assert_eq!(hit(PluginsChanged::User), roots);
    // 按路径段比：`/Users/dev/apple` 不在 `/Users/dev/app` 下面
    assert_eq!(
        hit(PluginsChanged::Workspace("/Users/dev/app".to_owned())),
        ["/Users/dev/app", "/Users/dev/app/web"]
    );
    assert_eq!(
        hit(PluginsChanged::Workspace("/Users/dev/app/web".to_owned())),
        ["/Users/dev/app/web"]
    );
}
