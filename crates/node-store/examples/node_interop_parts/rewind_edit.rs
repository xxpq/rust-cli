//! 互操作的第三十二段：编辑重发（REWIND，docs/specs/rust-cli/session-lifecycle.md §10.53）。
//!
//! `rewind-edit-write <storage-dir>`：照会话 actor 的 `ReplaceInput` 写的事实（与 `/rewind` 之后再输入相同）：两轮（first → one，second → two），
//! 一批 `RevertSet`（目标是第二轮的输入）与之后新输入的批次带同一个命令 id（编辑的命令），再写回答。输出会话 id 与新输入的 id。
use super::*;

/// 编辑的命令：截断那一批与新输入那一批都带它（session-lifecycle.md §10.53 的「落库」）。
const EDIT_COMMAND: &str = "cmd_e3";

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    let (u1, a1, u2, a2, u3, a3) = (
        message_id(),
        message_id(),
        message_id(),
        message_id(),
        message_id(),
        message_id(),
    );
    for (command, facts) in [
        (Some("cmd_e1"), vec![user(&u1, "first")]),
        (
            None,
            vec![reply(&a1, vec![text("one")], StopReason::EndTurn)],
        ),
        (Some("cmd_e2"), vec![user(&u2, "second")]),
        (
            None,
            vec![reply(&a2, vec![text("two")], StopReason::EndTurn)],
        ),
    ] {
        store.commit(&id, batch(command, facts)).await?;
    }
    let cursor = RevertCursor {
        target: u2,
        kept: vec![u1, a1],
        cut_after: a2,
        generation: 1,
    };
    store
        .commit(
            &id,
            batch(Some(EDIT_COMMAND), vec![Fact::RevertSet { cursor }]),
        )
        .await?;
    store
        .commit(
            &id,
            batch(Some(EDIT_COMMAND), vec![user(&u3, "second again")]),
        )
        .await?;
    store
        .commit(
            &id,
            batch(
                None,
                vec![reply(&a3, vec![text("edited")], StopReason::EndTurn)],
            ),
        )
        .await?;
    print(&json!({"session": id.as_str(), "edited": u3.as_str(), "command": EDIT_COMMAND}))
}
