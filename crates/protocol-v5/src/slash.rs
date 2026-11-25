//! `/` 面板里内核的命令（docs/specs/rust-cli/protocol-v5.md §60；docs/zcode-protocol-v5/15-ui-seam.md §31）：查询 `slash.commands`
//! （`catalog.slash@1`）。答的是这个工作区的内核此刻给的用户命令（`Kernel::user_commands`：skill、`commands/` 目录的命令、内置提示词
//! 命令），去掉 `hidden`（`user-invocable: false`），每条带来源 `kind`（§62.3），不带附加目录（同 v4 的 `workspace/readPresentation`，session-dirs.md §8）。
//!
//! ```text
//! actor: query slash.commands ─ 校验目标 ─► 任务：Workspaces::open（已组装的直接取）→ user_commands（按文件当前的内容，ext.md §2.10）
//! 任务 ─► Event::SlashCommands ─► actor 直接写出回复（不排进出站队列：查询结果与帧不保证先后，同 sessions.facts）
//! ```
//!
//! 没有 topic、不缓存：每一次查询都重新读，文件改了下一次就是新的（界面在 `/` 面板打开时查，15 §31.3）。

use serde_json::Value;

use crate::errors::{Fault, protocol_error, to_rpc};
use rust_cli_kernel_api::user_command::UserCommandKind;

use crate::generated::catalog_slash_v1::{
    QuerySlashCommandsResult, SlashCommandEntry, SlashCommandEntryKind,
};
use crate::generated::contract::codes;
use crate::generated::core_v1::{QueryEnvelope, QueryResult, QueryResultOk};
use crate::server::{Actor, Event};
use crate::topics;

/// 查询类型（catalog.slash@1）。
pub(crate) const SLASH_COMMANDS: &str = "slash.commands";

/// 在途的 `slash.commands`：`shutdown` 的回复等它们回来（03 §3.4）。
#[derive(Default)]
pub(crate) struct SlashReads {
    pub(crate) in_flight: usize,
}

fn fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

impl Actor {
    /// `slash.commands`：校验目标，交给任务（组装工作区、读文件都不在 actor 里，§4.1），回复等 [`Event::SlashCommands`]。
    pub(crate) fn slash_commands(
        &mut self,
        reply: &Value,
        query: QueryEnvelope,
    ) -> Result<(), Fault> {
        let Some(key) = query.target.workspace_key else {
            return Err(fault(
                codes::PRECONDITION_WORKSPACE_REQUIRED,
                "slash.commands needs target.workspaceKey",
            ));
        };
        topics::local_workspace(&key)
            .map_err(|message| fault(codes::PRECONDITION_INVALID, message))?;
        let Some((workspaces, _)) = self.sources() else {
            return Err(fault(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                "the workspace kernels are not ready yet",
            ));
        };
        self.slash.in_flight += 1;
        let (events, reply) = (self.events.clone(), reply.clone());
        drop(
            self.spawner
                .spawn("protocol-v5-slash-commands", async move {
                    let result = match workspaces.open(&key).await {
                        Ok(built) => {
                            let commands = built
                                .kernel
                                .user_commands(&workspaces.workspace(&key), &[])
                                .await
                                .into_iter()
                                // `user-invocable: false` 不进 `/` 面板，手输时内核给提示（ext.md §2.7）
                                .filter(|command| !command.hidden)
                                .map(|command| SlashCommandEntry {
                                    name: command.name,
                                    description: command.description,
                                    argument_hint: command.argument_hint,
                                    // 来源：界面照 v4 分区（skill 进技能区与 `$` 面板，命令进命令区，§62.3）
                                    kind: match command.kind {
                                        UserCommandKind::Skill => SlashCommandEntryKind::Skill,
                                        UserCommandKind::Command => SlashCommandEntryKind::Command,
                                    },
                                })
                                .collect();
                            let data = serde_json::to_value(QuerySlashCommandsResult { commands })
                                .unwrap_or(Value::Null);
                            Ok(serde_json::to_value(QueryResult::Ok(QueryResultOk {
                                data,
                                cursor: None,
                            }))
                            .unwrap_or(Value::Null))
                        }
                        // 组装失败（同会话的冷读，session/lane.rs 的 `failed`）
                        Err(message) => Err(fault(codes::INTERNAL_FAILED, message)),
                    };
                    let _ = events.send(Event::SlashCommands { reply, result });
                }),
        );
        Ok(())
    }

    /// 任务回来了：直接写出回复。
    pub(crate) async fn slash_answered(&mut self, reply: Value, result: Result<Value, Fault>) {
        self.slash.in_flight -= 1;
        self.peer
            .respond(&reply, &result.map_err(|error| to_rpc(&error)))
            .await;
        self.finish_shutdown();
    }
}

#[cfg(test)]
#[path = "slash_tests.rs"]
mod tests;
