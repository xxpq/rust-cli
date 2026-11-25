use serde_json::json;

use super::*;
use crate::core::catalog::parse;

fn catalogs() -> Vec<Catalog> {
    vec![
        parse(&json!({
            "name": "mkt",
            "allowCrossMarketplaceDependenciesOn": ["shared"],
            "plugins": [
                {"name": "app", "dependencies": ["lib", "util@shared"]},
                {"name": "lib", "dependencies": ["util@shared"]},
                {"name": "loop-a", "dependencies": ["loop-b"]},
                {"name": "loop-b", "dependencies": ["loop-a"]},
                {"name": "needs-missing", "dependencies": ["nope"]},
                {"name": "needs-blocked", "dependencies": ["x@blocked"]}
            ]
        }))
        .unwrap(),
        parse(&json!({"name": "shared", "plugins": [{"name": "util"}]})).unwrap(),
        parse(&json!({"name": "blocked", "plugins": [{"name": "x"}]})).unwrap(),
    ]
}

fn run(root: &str) -> Result<Vec<String>, StoreError> {
    let all = catalogs();
    closure(&PluginId::parse(root).unwrap(), |name| {
        all.iter().find(|c| c.name == name)
    })
    .map(|members| members.into_iter().map(|m| m.id.to_string()).collect())
}

#[test]
fn dependencies_come_first_and_once() {
    assert_eq!(
        run("app@mkt").unwrap(),
        ["util@shared", "lib@mkt", "app@mkt"]
    );
}

#[test]
fn errors_name_the_problem_like_node() {
    assert_eq!(
        run("loop-a@mkt").unwrap_err(),
        StoreError::DependencyCycle(
            "Plugin dependency cycle: loop-a@mkt -> loop-b@mkt -> loop-a@mkt".into()
        )
    );
    assert_eq!(
        run("needs-missing@mkt").unwrap_err(),
        StoreError::DependencyMissing(
            "Dependency not found: nope@mkt required by needs-missing@mkt".into()
        )
    );
    assert!(matches!(
        run("needs-blocked@mkt").unwrap_err(),
        StoreError::CrossMarketplace(_)
    ));
    assert_eq!(
        run("ghost@mkt").unwrap_err(),
        StoreError::PluginNotFound("ghost@mkt".into())
    );
    assert_eq!(
        run("x@none").unwrap_err(),
        StoreError::MarketplaceNotFound("none".into())
    );
}
