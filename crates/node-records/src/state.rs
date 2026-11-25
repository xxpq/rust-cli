//! 会话当前状态的读取：模型选择、执行状态、当前轮（docs/specs/rust-cli/node-store.md §4）。
//!
//! 写入 assistant 记录要用的 provider、模式从库里读，库是唯一事实源：同一事务里先提交的事实（例如同一批的
//! `ModelSelected`）立刻可见，Node 续写过的会话也一样。父消息与 `anchor.turnId` 由批次的回合锚点给出
//! （`facts::Commit::current_user`，修复线 F6）；[`current_user`] 只在没有锚点时与冷读取时用。
use crate::rows::Rows;
use serde_json::{Value, json};

use crate::codecs::{decode_message, parse_model_selection};
use crate::mode::{self, Execution};
use crate::records::{EXECUTION_STATE_ENTRY, MODEL_SELECTION_ENTRY};
use crate::rows::{Result, SessionRow};

/// 最后一条 entry 的 data。
pub fn last_entry(conn: &dyn Rows, session: &str, kind: &str) -> Result<Option<Value>> {
    Ok(conn
        .entries(session, Some(kind))?
        .pop()
        .map(|entry| entry.data))
}

/// 会话的模型选择（Node 形状）：最后一条 `runtime/model_selection`，严格结构优先，否则只取
/// `providerId` 与 `modelId`（docs/specs/rust-m11-node-storage.md §6.1）。
pub fn selection(conn: &dyn Rows, session: &str) -> Result<Option<Value>> {
    let Some(data) = last_entry(conn, session, MODEL_SELECTION_ENTRY)? else {
        return Ok(None);
    };
    if let Some(parsed) = parse_model_selection(&data) {
        return Ok(Some(parsed));
    }
    let text = |key: &str| {
        data[key]
            .as_str()
            .map(rust_cli_base::js::string::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Ok(text("providerId")
        .zip(text("modelId"))
        .map(|(provider, model)| json!({"providerId": provider, "modelId": model})))
}

/// 会话的执行状态（Node `resumeFromStore` 的持久部分）：以会话行 `permission.mode` 为底，最后一条
/// 合法的 `runtime/execution_state` 整体覆盖。两者都没有时为 `None`。
pub fn execution(conn: &dyn Rows, row: &SessionRow) -> Result<Option<Execution>> {
    let base = row
        .permission
        .as_ref()
        .and_then(|permission| permission["mode"].as_str())
        .map(|mode| mode::resolve(Some(mode), &Execution::default()));
    let saved = last_entry(conn, &row.id, EXECUTION_STATE_ENTRY)?
        .as_ref()
        .and_then(mode::parse);
    Ok(saved.or(base))
}

/// 当前轮：最后一条真实用户输入（`semantics.origin = real_user`）的 id 与 `anchor.turnId`。
/// 它是 assistant 的 `parentID`（Node `currentUserMessageId`）。非法 JSON 视为不匹配。
pub fn current_user(conn: &dyn Rows, session: &str) -> Result<Option<(String, Option<String>)>> {
    // 本机 SQLite 的两段查询见 node-store `rows.rs`（性能修复 L21）。
    let stored = conn.current_user_row(session)?;
    let Some((id, data)) = stored else {
        return Ok(None);
    };
    let info = decode_message(&data, &id, session).map_err(crate::storage)?;
    let turn = info["anchor"]["turnId"].as_str().map(str::to_owned);
    Ok(Some((id, turn)))
}

/// 本会话里的一条消息（Node `MessageInfo`）；不存在时为 `None`。
pub fn message(conn: &dyn Rows, session: &str, id: &str) -> Result<Option<Value>> {
    let data = conn.message_data(session, id)?;
    data.map(|data| decode_message(&data, id, session).map_err(crate::storage))
        .transpose()
}
