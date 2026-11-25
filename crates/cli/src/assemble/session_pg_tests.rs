use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::EnvironmentId;

use super::*;

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

#[test]
fn the_project_follows_the_workspace() {
    let a = project(&workspace("/repo"));
    assert_eq!(a.id, project(&workspace("/repo")).id);
    assert_ne!(a.id, project(&workspace("/other")).id);
    assert!(a.id.starts_with("ws_") && a.id.len() == 35, "{}", a.id);
}
