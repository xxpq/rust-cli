//! 任务清单进 `state.plan`（protocol-v5.md §51.2，K-S9）：由视图建与按 `TasksChanged` 更新是同一个函数，所以冷读与活会话同值；
//! 每项 `{id, text: subject, status}`，按清单的顺序；`updatedAt` 恒 0。

use rust_cli_kernel_api::agents::{TaskRecord, TaskStatus};
use rust_cli_kernel_api::session_api::KernelEvent;
use serde_json::{Value, json};

use super::kit::{apply, live, origin, publisher, view};
use super::*;

/// 线上的 state 键（契约 `SESSION_STATE_KEY_SCHEMAS.plan`）。
const PLAN: &str = "plan";

fn task(id: &str, subject: &str, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id: id.to_owned(),
        subject: subject.to_owned(),
        description: "what to do".to_owned(),
        active_form: Some("Doing it".to_owned()),
        owner: None,
        status,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
        metadata: None,
    }
}

fn tasks() -> Vec<TaskRecord> {
    vec![
        task("1", "Write the spec", TaskStatus::Completed),
        task("2", "Write the tests", TaskStatus::InProgress),
        task("3", "Ship it", TaskStatus::Pending),
    ]
}

fn plan(publisher: &Publisher) -> Value {
    publisher.state().get(PLAN).cloned().unwrap_or(Value::Null)
}

#[test]
fn the_view_and_the_event_give_the_same_plan() {
    let expected = json!({"items": [
        {"id": "1", "text": "Write the spec", "status": "completed"},
        {"id": "2", "text": "Write the tests", "status": "in_progress"},
        {"id": "3", "text": "Ship it", "status": "pending"},
    ], "updatedAt": 0});
    // 冷读：由带清单的视图建
    let cold = SessionView {
        tasks: tasks(),
        ..view(Vec::new(), Vec::new())
    };
    let (_, content) = Projection::build(&cold, origin(false));
    let mut built = publisher();
    built.adopt(content);
    assert_eq!(plan(&built), expected);
    // 活会话：空清单起步，`TasksChanged` 带完整清单（agents 写的 JSON）
    let (mut projection, mut publisher) = live();
    assert_eq!(plan(&publisher), json!({"items": [], "updatedAt": 0}));
    apply(
        &mut publisher,
        &mut projection,
        vec![KernelEvent::TasksChanged {
            tasks: json!(tasks()),
        }],
    );
    assert_eq!(plan(&publisher), expected);
}

/// 删除的任务不在清单里；清单清空之后 `plan` 回到空的项（Todo 一节消失）。
#[test]
fn a_shorter_list_replaces_the_plan() {
    let (mut projection, mut publisher) = live();
    apply(
        &mut publisher,
        &mut projection,
        vec![
            KernelEvent::TasksChanged {
                tasks: json!(tasks()),
            },
            KernelEvent::TasksChanged {
                tasks: json!([task("2", "Write the tests", TaskStatus::Completed)]),
            },
        ],
    );
    assert_eq!(
        plan(&publisher),
        json!({"items": [{"id": "2", "text": "Write the tests", "status": "completed"}], "updatedAt": 0})
    );
    apply(
        &mut publisher,
        &mut projection,
        vec![KernelEvent::TasksChanged { tasks: json!([]) }],
    );
    assert_eq!(plan(&publisher), json!({"items": [], "updatedAt": 0}));
}
