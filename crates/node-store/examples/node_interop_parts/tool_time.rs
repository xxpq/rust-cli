//! 互操作的第二十九段：工具调用的开工与结束（docs/specs/rust-cli/node-store.md §4.6 第 4 条，session-lifecycle.md §10.37，TOOL-TIME）。
//!
//! `tool-time-write <storage-dir>`：一条回复里两个调用，一个成功、一个失败；各自的结果与它的计量（`Fact::ToolCalled`）同一批（主循环的写法），
//! 计量的开工与结束早于结果消息的创建时刻，最后一条回答。输出会话 id、回复 id 与两个调用的 `ToolCall::time`。
use rust_cli_kernel_api::metering::{CallStatus, ToolCall};

use super::*;

/// 计量的开工比写入早这么多（毫秒）：结束（开工之后 `RUN_MS`）也早于结果消息的创建时刻，库里的 `time.end` 是哪一个一看便知。
const EARLIER_MS: u64 = 10_000;
const RUN_MS: u64 = 250;

fn metered(call: &str, started_at_ms: u64, status: CallStatus) -> ToolCall {
    ToolCall {
        tool_use: ToolUseId::new(call),
        name: "Read".into(),
        started_at_ms,
        completed_at_ms: started_at_ms + RUN_MS,
        status,
        output_bytes: 1,
    }
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    let mut current = None;
    let (ask, reply_id, answer) = (message_id(), message_id(), message_id());
    let base = now_ms() - EARLIER_MS;
    let ok = metered("toolu_time_ok", base, CallStatus::Completed);
    let failed = metered("toolu_time_fail", base + RUN_MS, CallStatus::Error);
    let read = |file: &str| json!({"file_path": format!("{WORKSPACE}/{file}")});
    let steps = vec![
        vec![user(&ask, "Read two files")],
        vec![reply(
            &reply_id,
            vec![
                call("toolu_time_ok", "Read", &read("a.txt")),
                call("toolu_time_fail", "Read", &read("missing.txt")),
            ],
            StopReason::ToolUse,
        )],
        vec![
            result(&reply_id, "toolu_time_ok", vec![said("a")], false),
            Fact::ToolCalled { call: ok.clone() },
        ],
        vec![
            result(
                &reply_id,
                "toolu_time_fail",
                vec![said("File does not exist.")],
                true,
            ),
            Fact::ToolCalled {
                call: failed.clone(),
            },
        ],
        vec![reply(&answer, vec![text("Read.")], StopReason::EndTurn)],
    ];
    for facts in steps {
        store.commit(&id, anchored(&mut current, facts)).await?;
    }
    print(&json!({
        "session": id.as_str(),
        "reply": reply_id.as_str(),
        "times": [ok.time(), failed.time()],
    }))
}
