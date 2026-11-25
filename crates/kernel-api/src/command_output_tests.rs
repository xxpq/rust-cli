//! Bash 结果里只给界面的输出头部（shell-permissions.md §1.3.1）。
use super::*;
use crate::tool::text;

#[test]
fn the_head_is_read_back_next_to_other_keys() {
    let mut output = text("<persisted-output>".to_owned());
    output.display = serde_json::json!({ "checkpoint": { "path": "/w/a.txt" } });
    let head = CommandOutputHead {
        head: "line-1\nline-2".to_owned(),
        output_file: "/tmp/zcode-1000/sess_1/tasks/b1.output".to_owned(),
    };
    let output = head.clone().attach(output);
    assert_eq!(output.display["checkpoint"]["path"], "/w/a.txt");
    assert_eq!(CommandOutputHead::of(&output), Some(head));
}

#[test]
fn a_result_without_the_key_has_no_head() {
    assert_eq!(CommandOutputHead::of(&text("done".to_owned())), None);
}

/// 一次调用的头部是落库与冷读的单位（session-lifecycle.md §10.34）：serde 往返不变；工具结果有头部时给出同批的事实。
#[test]
fn a_call_head_round_trips_and_becomes_the_fact() {
    let head = CommandOutputHead {
        head: "line-1".to_owned(),
        output_file: "/tmp/zcode-1000/sess_1/tasks/b1.output".to_owned(),
    };
    let call = ToolOutputHead {
        tool_use: crate::ids::ToolUseId::new("toolu_1"),
        head: head.clone(),
    };
    let value = serde_json::to_value(&call).unwrap();
    assert_eq!(
        serde_json::from_value::<ToolOutputHead>(value).unwrap(),
        call
    );
    let output = head.attach(text("<persisted-output>".to_owned()));
    assert_eq!(
        CommandOutputHead::fact(&crate::ids::ToolUseId::new("toolu_1"), &output),
        Some(crate::store::Fact::OutputHead { head: call })
    );
    assert_eq!(
        CommandOutputHead::fact(
            &crate::ids::ToolUseId::new("toolu_2"),
            &text("done".to_owned())
        ),
        None
    );
}
