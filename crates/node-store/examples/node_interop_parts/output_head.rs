//! 互操作的第二十八段：Bash 的输出头部（docs/specs/rust-cli/node-store.md §4.6 第 7 条，PROJ-GAPS）。
//!
//! `output-head-write <storage-dir>`：一条回复里两个 Bash 调用，一个成功的大输出（结果是给模型的预览）、一个失败的大输出（退出码 3），
//! 各自的结果与它的 `Fact::OutputHead` 同一批（会话 actor 的写法：排在结果之后），最后一条回答。输出会话 id 与两个头部。
use rust_cli_kernel_api::command_output::{CommandOutputHead, ToolOutputHead};

use super::*;

fn head(call: &str, head: &str) -> ToolOutputHead {
    ToolOutputHead {
        tool_use: ToolUseId::new(call),
        head: CommandOutputHead {
            head: head.into(),
            output_file: format!("/tmp/zcode-1000/interop/tasks/{call}.output"),
        },
    }
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    let mut current = None;
    let (ask, reply_id, answer) = (message_id(), message_id(), message_id());
    let ok = head("toolu_head_ok", "head-line-0\nhead-line-1");
    let failed = head("toolu_head_fail", "head-line-0");
    let steps = vec![
        vec![user(&ask, "Print a lot")],
        vec![reply(
            &reply_id,
            vec![
                call("toolu_head_ok", "Bash", &json!({"command": "seq 100000"})),
                call(
                    "toolu_head_fail",
                    "Bash",
                    &json!({"command": "seq 100000; exit 3"}),
                ),
            ],
            StopReason::ToolUse,
        )],
        vec![
            result(
                &reply_id,
                "toolu_head_ok",
                vec![said(
                    "<persisted-output>\nOutput too large\n</persisted-output>",
                )],
                false,
            ),
            Fact::OutputHead { head: ok.clone() },
        ],
        vec![
            result(
                &reply_id,
                "toolu_head_fail",
                vec![said("Exit code 3\nhead-line-0")],
                true,
            ),
            Fact::OutputHead {
                head: failed.clone(),
            },
        ],
        vec![reply(&answer, vec![text("Printed.")], StopReason::EndTurn)],
    ];
    for facts in steps {
        store.commit(&id, anchored(&mut current, facts)).await?;
    }
    print(&json!({
        "session": id.as_str(),
        "reply": reply_id.as_str(),
        "heads": [ok, failed],
    }))
}
