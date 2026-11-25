//! 本机项目规则集的读与“总是允许”的写（shell-permissions.md §5）。
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};
use rust_cli_kernel_api::store::LocalSettings;
use rust_cli_kernel_api::store::{Fact, SessionStore};

use crate::tests::{TestStore, batch, meta};

const ROOT: &str = "/Users/dev/repo";

fn rule(tool: &str, content: Option<&str>) -> PermissionRule {
    PermissionRule {
        tool: tool.to_owned(),
        content: content.map(str::to_owned),
    }
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(ROOT),
    }
}

fn setting(test: &TestStore) -> Vec<Vec<serde_json::Value>> {
    test.rows("select scope, scope_id, namespace, key, value, schema_version, time_created, time_updated from local_setting")
}

#[tokio::test]
async fn always_allow_rules_follow_the_node_ruleset() {
    let test = TestStore::new();
    let session = SessionId::new("sess_rules");
    test.store
        .create(
            meta("sess_rules", ROOT, ""),
            crate::tests::batch(None, vec![]),
        )
        .await
        .unwrap();
    assert_eq!(
        test.store.project_rules(&workspace()).await.unwrap(),
        Default::default()
    );
    let added = Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![rule("Bash", Some("git status:*")), rule("Read", None)],
    };
    test.store
        .commit(&session, batch(None, vec![added]))
        .await
        .unwrap();
    let rows = setting(&test);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], "project");
    assert_eq!(rows[0][1], "proj_users-dev-repo");
    assert_eq!(
        rows[0][4],
        r#"{"version":1,"allow":[{"toolName":"Bash","ruleContent":"git status:*"},{"toolName":"Read"}]}"#
    );
    assert_eq!(rows[0][5], 1);
    // 再加一条重复的与一条新的：去重、追加，时间只改 time_updated。
    test.source.at(2000);
    let again = Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![
            rule("Read", None),
            rule("WebFetch", Some("domain:example.com")),
        ],
    };
    test.store
        .commit(&session, batch(None, vec![again]))
        .await
        .unwrap();
    let rows = setting(&test);
    assert_eq!(
        rows[0][4],
        r#"{"version":1,"allow":[{"toolName":"Bash","ruleContent":"git status:*"},{"toolName":"Read"},{"toolName":"WebFetch","ruleContent":"domain:example.com"}]}"#
    );
    assert_eq!(
        (rows[0][6].clone(), rows[0][7].clone()),
        (1000.into(), 2000.into())
    );
    let read = test.store.project_rules(&workspace()).await.unwrap();
    assert_eq!(read.allow.len(), 3);
    assert_eq!(read.allow[0], rule("Bash", Some("git status:*")));
}

#[tokio::test]
async fn the_legacy_permission_table_is_read_and_keys_keep_their_order() {
    let test = TestStore::new();
    let session = SessionId::new("sess_legacy");
    test.store
        .create(
            meta("sess_legacy", ROOT, ""),
            crate::tests::batch(None, vec![]),
        )
        .await
        .unwrap();
    test.connection()
        .execute(
            "insert into permission (project_id, time_created, time_updated, data) values (?, 1, 1, ?)",
            rusqlite::params![
                "proj_users-dev-repo",
                r#"{"ask":[{"toolName":"Bash","ruleContent":"rm:*"}],"deny":[{"toolName":"Write"}],"extra":true}"#
            ],
        )
        .unwrap();
    let read = test.store.project_rules(&workspace()).await.unwrap();
    assert_eq!(read.ask, [rule("Bash", Some("rm:*"))]);
    assert_eq!(read.deny, [rule("Write", None)]);
    let added = Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![rule("Glob", None)],
    };
    test.store
        .commit(&session, batch(None, vec![added]))
        .await
        .unwrap();
    // `{...当前, version: 1, allow}`：已有的键保持原位，新键追加在后面。
    assert_eq!(
        setting(&test)[0][4],
        r#"{"ask":[{"toolName":"Bash","ruleContent":"rm:*"}],"deny":[{"toolName":"Write"}],"extra":true,"version":1,"allow":[{"toolName":"Glob"}]}"#
    );
}

#[tokio::test]
async fn an_unreadable_ruleset_is_not_overwritten() {
    let test = TestStore::new();
    let session = SessionId::new("sess_bad");
    test.store
        .create(
            meta("sess_bad", ROOT, ""),
            crate::tests::batch(None, vec![]),
        )
        .await
        .unwrap();
    test.connection()
        .execute(
            "insert into local_setting values ('project', 'proj_users-dev-repo', 'permission', 'ruleset', '[1]', 1, 1, 1)",
            [],
        )
        .unwrap();
    assert_eq!(
        test.store.project_rules(&workspace()).await.unwrap(),
        Default::default()
    );
    let added = Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![rule("Glob", None)],
    };
    assert!(
        test.store
            .commit(&session, batch(None, vec![added]))
            .await
            .is_err()
    );
    assert_eq!(setting(&test)[0][4], "[1]");
}

/// `/permissions` 的加与删（tui-manage.md §1.1）：不需要会话，键序与去重同 Node，删掉后留空数组。
#[tokio::test]
async fn permissions_changes_add_and_remove_in_one_transaction() {
    use rust_cli_kernel_api::store::ProjectRulesChange;
    let test = TestStore::new();
    // 先经存储打开库（建库与迁移）。
    test.store.project_rules(&workspace()).await.unwrap();
    test.connection()
        .execute(
            "insert into local_setting values ('project', 'proj_users-dev-repo', 'permission', 'ruleset', ?, 1, 1, 1)",
            [r#"{"mode":"edit","allow":[{"toolName":"Read"}]}"#],
        )
        .unwrap();
    test.source.at(3000);
    let change =
        |behavior, add: Vec<PermissionRule>, remove: Vec<PermissionRule>| ProjectRulesChange {
            behavior,
            add,
            remove,
        };
    let rules = test
        .store
        .change_project_rules(
            &workspace(),
            change(
                RuleBehavior::Deny,
                vec![rule("Bash", Some("rm *")), rule("Bash", Some("rm *"))],
                Vec::new(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(rules.deny, [rule("Bash", Some("rm *"))]);
    assert_eq!(
        setting(&test)[0][4],
        r#"{"mode":"edit","allow":[{"toolName":"Read"}],"version":1,"deny":[{"toolName":"Bash","ruleContent":"rm *"}]}"#
    );
    assert_eq!(setting(&test)[0][7], 3000);
    let rules = test
        .store
        .change_project_rules(
            &workspace(),
            change(RuleBehavior::Allow, Vec::new(), vec![rule("Read", None)]),
        )
        .await
        .unwrap();
    assert!(rules.allow.is_empty());
    assert_eq!(
        setting(&test)[0][4],
        r#"{"mode":"edit","allow":[],"version":1,"deny":[{"toolName":"Bash","ruleContent":"rm *"}]}"#
    );
    // 没有这一行时从空对象开始；当前值不是对象时不覆盖。
    let other = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/other"),
    };
    let rules = test
        .store
        .change_project_rules(
            &other,
            change(RuleBehavior::Ask, vec![rule("WebFetch", None)], Vec::new()),
        )
        .await
        .unwrap();
    assert_eq!(rules.ask, [rule("WebFetch", None)]);
    test.connection()
        .execute(
            "update local_setting set value = '[1]' where scope_id = 'proj_users-dev-repo'",
            [],
        )
        .unwrap();
    assert!(
        test.store
            .change_project_rules(
                &workspace(),
                change(RuleBehavior::Allow, vec![rule("Glob", None)], Vec::new())
            )
            .await
            .is_err()
    );
}

/// hook 的 `replaceRules` / `removeRules` 落在本机（permission-updates.md §2.3）：只动那一类，其余键与键序不变，去重同 Node。
#[tokio::test]
async fn hook_updates_replace_and_remove_one_kind_of_rules() {
    let test = TestStore::new();
    let session = SessionId::new("sess_edit");
    test.store
        .create(
            meta("sess_edit", ROOT, ""),
            crate::tests::batch(None, vec![]),
        )
        .await
        .unwrap();
    test.connection()
        .execute(
            "insert into local_setting values ('project', 'proj_users-dev-repo', 'permission', 'ruleset', ?, 1, 1, 1)",
            [r#"{"allow":[{"toolName":"Read"},{"toolName":"Glob"}],"extra":true,"deny":[{"toolName":"Write"}]}"#],
        )
        .unwrap();
    let facts = vec![
        Fact::PermissionRulesReplaced {
            behavior: RuleBehavior::Allow,
            rules: vec![rule("Bash", Some("ls *")), rule("Bash", Some("ls *"))],
        },
        Fact::PermissionRulesRemoved {
            behavior: RuleBehavior::Deny,
            rules: vec![rule("Write", None), rule("Edit", None)],
        },
    ];
    test.store
        .commit(&session, batch(None, facts))
        .await
        .unwrap();
    assert_eq!(
        setting(&test)[0][4],
        r#"{"allow":[{"toolName":"Bash","ruleContent":"ls *"}],"extra":true,"deny":[],"version":1}"#
    );
    let read = test.store.project_rules(&workspace()).await.unwrap();
    assert_eq!(read.allow, [rule("Bash", Some("ls *"))]);
    assert!(read.deny.is_empty());
}

/// 记住的附加工作目录（permission-updates.md §2.3、§6）：`additionalDirectories` 追加去重、可以去掉，读回成内核缺省用的值；
/// 规则与不认识的键原样。
#[tokio::test]
async fn remembered_directories_live_in_the_ruleset() {
    let test = TestStore::new();
    let session = SessionId::new("sess_dirs");
    test.store
        .create(
            meta("sess_dirs", ROOT, ""),
            crate::tests::batch(None, vec![]),
        )
        .await
        .unwrap();
    test.connection()
        .execute(
            "insert into local_setting values ('project', 'proj_users-dev-repo', 'permission', 'ruleset', ?, 1, 1, 1)",
            [r#"{"version":1,"allow":[{"toolName":"Read"}],"extra":true}"#],
        )
        .unwrap();
    let dir = |path: &str| EnvPath::new(path);
    let remembered = Fact::DirectoriesRemembered {
        directories: vec![
            dir("/Users/dev/a"),
            dir("/Users/dev/b"),
            dir("/Users/dev/a"),
        ],
    };
    test.store
        .commit(&session, batch(None, vec![remembered]))
        .await
        .unwrap();
    assert_eq!(
        setting(&test)[0][4],
        r#"{"version":1,"allow":[{"toolName":"Read"}],"extra":true,"additionalDirectories":["/Users/dev/a","/Users/dev/b"]}"#
    );
    let forgotten = Fact::DirectoriesForgotten {
        directories: vec![dir("/Users/dev/a")],
    };
    test.store
        .commit(&session, batch(None, vec![forgotten]))
        .await
        .unwrap();
    let read = test.store.project_rules(&workspace()).await.unwrap();
    assert_eq!(read.additional_directories, [dir("/Users/dev/b")]);
    assert_eq!(read.allow, [rule("Read", None)]);
    // 没有会话时（`/add-dir` 的草稿）经 `LocalSettings::remember_directories`：同一种写法。
    test.store
        .remember_directories(&workspace(), vec![dir("/Users/dev/c"), dir("/Users/dev/b")])
        .await
        .unwrap();
    assert_eq!(
        test.store
            .project_rules(&workspace())
            .await
            .unwrap()
            .additional_directories,
        [dir("/Users/dev/b"), dir("/Users/dev/c")]
    );
    // 值不是字符串的项读成没有（Node 写的值只会是字符串，读法不因一项坏值丢掉别的）。
    test.connection()
        .execute(
            "update local_setting set value = ? where key = 'ruleset'",
            [r#"{"additionalDirectories":["/Users/dev/d",3]}"#],
        )
        .unwrap();
    assert_eq!(
        test.store
            .project_rules(&workspace())
            .await
            .unwrap()
            .additional_directories,
        [dir("/Users/dev/d")]
    );
}
