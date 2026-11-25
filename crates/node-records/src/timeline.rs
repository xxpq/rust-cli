//! Node `persistAssistantTimelinePartForSession`（`timeline-persistence.ts`）写的消息：只给界面看的 assistant（`timeline_event`，
//! 不进模型上下文），模型、模式与路径取会话。fork 的分割线（`fork.rs`）与压缩的标记（`compaction.rs`，compact.md §6）都写它，
//! 各自再写自己的 part。
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Value, json};

use crate::facts::Commit;
use crate::ids::message_id;
use crate::{records, state, storage};

/// 写这条消息，返回它的 id。`selection` 是 Node 写法的模型选择（`modelId` / `providerId`），没有时这两个键不写（Node 是 undefined）；
/// `parent` 是它跟在哪条消息之后，没有时是它自己（Node `parentID ?? latestConversationMessageId ?? messageID`）。
pub(crate) fn save(
    ctx: &Commit<'_>,
    directory: &str,
    selection: Option<&Value>,
    parent: Option<&str>,
) -> Result<String, StoreError> {
    let id = message_id(ctx.now, &(ctx.uuid)());
    let row = ctx
        .conn
        .session(ctx.session)?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    let execution = state::execution(ctx.conn, &row)?.unwrap_or_default();
    let now = ctx.now;
    let mut info = json!({"id": id, "sessionID": ctx.session, "role": "assistant",
        "time": {"created": now, "completed": now}, "parentID": parent.unwrap_or(&id)});
    if let Some(selection) = selection {
        info["modelId"] = selection["modelId"].clone();
        info["providerId"] = selection["providerId"].clone();
    }
    info["mode"] = execution.mode.into();
    info["planEnabled"] = execution.plan.into();
    info["agent"] = records::AGENT.into();
    info["path"] = json!({"cwd": directory, "root": directory});
    info["cost"] = 0.into();
    info["tokens"] = records::tokens(None);
    info["finish"] = "completed".into();
    info["semantics"] = json!({"origin": "system", "kind": "timeline_event", "uiVisibility": "visible",
        "providerVisibility": "hidden", "transcriptVisibility": "visible"});
    ctx.conn.save_message(&info, now as i64)?;
    Ok(id)
}
