//! 一次运行看到的工具（tools-more.md §3.5、session-lifecycle.md §10.15）：清单、快照、查找都去掉名单里的工具，名单原样交给
//! 派生的子代理。

use std::sync::Arc;

use super::RunTools;
use crate::ids::SessionId;
use crate::orchestration::ToolRegistry;
use crate::testing::{FakeTool, ListTools};
use crate::tool::Tool;

fn names(tools: &[Arc<dyn Tool>]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name()).collect()
}

#[test]
fn the_run_leaves_out_the_tools_it_disallows() {
    let registry: Arc<dyn ToolRegistry> = Arc::new(ListTools(vec![
        Arc::new(FakeTool::new("Read", true, "content")),
        Arc::new(FakeTool::new("CronCreate", false, "created")),
    ]));
    let run = RunTools::new(registry, &["CronCreate".to_owned()]);
    assert_eq!(names(&run.tools()), ["Read"]);
    assert_eq!(
        names(&run.snapshot(&SessionId::new("sess_1")).tools),
        ["Read"]
    );
    assert!(run.find("CronCreate").is_none());
    assert!(run.find("Read").is_some());
    assert!(run.hides("CronCreate"));
    assert!(!run.hides("Read"));
    assert_eq!(run.disallowed(), ["CronCreate"]);
}
