//! 不带参数的 `/rename`：模型起名（docs/specs/rust-cli/titles.md §8.2）。会话 actor 是标题与来源的唯一
//! owner：对话文本在 actor 里取，起名任务只算名字，回到 actor 后才按 `Rename` 提交、发事件、回执。
//! 同一时间只有一个起名任务；起名期间有新的改名（`Rename` 或 hook）时不写，回执 `superseded` 的文本。

use rust_cli_base::{Task, TaskError};
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::model::small_fast_model;
use rust_cli_kernel_api::session_api::{Ack, CommandResult};
use rust_cli_kernel_api::side_request::SideReply;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, rejected};
use crate::rename_text::{clean_name, conversation, parse_name, request};

/// 没有对话与解析失败用同一段文本。
const NO_NAME: &str =
    "Could not generate a name: no conversation context yet. Usage: /rename <name>";
const BUSY: &str = "A session name is being generated.";

/// 在跑的起名任务（actor 的 `naming` 字段）。
pub(crate) struct NameJob {
    command: CommandId,
    reply: crate::ledger::Reply,
    /// 起名开始时的改名计数（`TitleState::renames`）。
    renames: u64,
    task: Task<Option<String>>,
}

/// 起名任务的结果；没有任务时一直等（actor 的 select 分支）。
pub(crate) async fn name_done(job: &mut Option<NameJob>) -> Result<Option<String>, TaskError> {
    match job {
        Some(job) => (&mut job.task).await,
        None => std::future::pending().await,
    }
}

impl Actor {
    /// `GenerateName`：回执等名字（与代码回退相同，回执晚于命令）。
    pub(crate) fn generate_name(&mut self, command: CommandId, reply: crate::ledger::Reply) {
        if self.naming.is_some() {
            self.commands.settle(reply, rejected(Refusal::Busy, BUSY));
            return;
        }
        let text = conversation(&self.messages);
        if text.is_empty() {
            self.commands
                .settle(reply, rejected(Refusal::Unavailable, NO_NAME));
            return;
        }
        // 模型经小模型槽位，输入是 actor 的当前选择（model.md §6.1）。
        let mut selection = small_fast_model(&self.selection);
        let session = self.meta.id.clone();
        let model = self.deps.side_model.clone();
        let task = self.deps.spawner.spawn("session-name", async move {
            // 目录的输出上限：没有卡片的模型按它（MO-3）。
            let configured = model
                .capabilities(&selection)
                .await
                .and_then(|found| found.max_output_tokens);
            // 档位（model-options.md §4.4，D5）：配置路取 provider 的第一档（同 Node 的辅助调用），卡片路照会话的选择。
            let offered = model.reasoning_levels(&selection).await;
            selection.reasoning = rust_cli_prompts::models::side_reasoning(
                &selection.model,
                selection.reasoning.as_deref(),
                offered.as_deref(),
            );
            let request = request(&selection, &session, (&text, configured));
            let reply =
                match SideReply::collect(model.stream(request, CancellationToken::new())).await {
                    Ok(reply) => reply.text(),
                    Err(error) => {
                        tracing::warn!(%error, "session name generation failed");
                        return None;
                    }
                };
            parse_name(&reply).as_deref().and_then(clean_name)
        });
        self.naming = Some(NameJob {
            command,
            reply,
            renames: self.titles.renames(),
            task,
        });
    }

    /// 名字回到 actor：起名期间没有新的改名时按 `Rename` 写入。
    pub(crate) async fn on_name(&mut self, result: Result<Option<String>, TaskError>) {
        let Some(job) = self.naming.take() else {
            return;
        };
        let name = match result {
            Ok(Some(name)) => name,
            Ok(None) => {
                self.commands
                    .settle(job.reply, rejected(Refusal::Unavailable, NO_NAME));
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "the session name task ended without a result");
                self.commands
                    .settle(job.reply, rejected(Refusal::Unavailable, NO_NAME));
                return;
            }
        };
        let ack = if self.titles.renames() == job.renames {
            match self.rename(job.command, name.clone(), None).await {
                Ack::Accepted { .. } => Ack::Accepted {
                    result: CommandResult::Renamed {
                        message: format!("Session renamed to: {name}"),
                        name,
                    },
                },
                other => other,
            }
        } else {
            // `superseded`：新的改名先落地，保留它。
            let current = self.meta.title.clone();
            Ack::Accepted {
                result: CommandResult::Renamed {
                    message: format!("Session is named: {current} (a newer rename landed first)"),
                    name: current,
                },
            }
        };
        self.commands.settle(job.reply, ack);
    }
}

#[cfg(test)]
#[path = "rename_tests.rs"]
mod tests;
