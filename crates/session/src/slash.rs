//! 斜杠命令的展开（docs/specs/rust-cli/ext.md §2.7、session-lifecycle.md §3.1）：人类输入开的回合任务里，
//! 先经 [`UserCommands`] 展开，再跑 UserPromptSubmit。展开只给出文字与种类；这里决定消息 ID——第一条
//! 用户输入用 actor 预先生成的 prompt 消息 ID（checkpoint、标题与命令锚点都认它）。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::Origin;
use rust_cli_kernel_api::ids::{RunId, ToolUseId};
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::run_tools::RunTools;
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_kernel_api::user_command::{ExpandedKind, Expansion, UserCommands};

use crate::actor::Actor;

/// 回合要提交的输入：消息（按顺序）与是否调模型。
pub(crate) struct Inputs {
    pub messages: Vec<Message>,
    pub query: bool,
    /// 输入是普通输入（不是斜杠命令）：只有这时展开 `@` 提及（docs/specs/rust-cli/tui-input.md §1.1）。
    pub plain: bool,
}

impl Actor {
    /// 展开时 `!` 命令用的工具上下文：操作来源是用户（`Origin::User`），取消令牌是本回合的中止信号；询问与会话操作都经
    /// 这个回合的输出端。工具是主线程注册表在这个会话的视图、没有禁用名单（这些调用不是模型发起的，tools-more.md §3.5）。
    pub(crate) fn command_context(
        &self,
        run: &RunId,
        abort: &Abort,
        sink: Arc<crate::sink::ActorSink>,
    ) -> ToolUseContext {
        let operation = self.deps.ids.operation();
        ToolUseContext {
            session: self.meta.id.clone(),
            run: run.clone(),
            tool_use: ToolUseId::new(operation.as_str()),
            agent: None,
            root: None,
            workspace: self.meta.workspace.clone(),
            mode: self.mode(),
            additional_directories: self.additional_directories.clone(),
            session_rules: self.session_rules.clone(),
            tools: Arc::new(RunTools::new(
                self.deps.tools.clone().view(&self.meta.id),
                &[],
            )),
            cancel: abort.token().clone(),
            machine: self.deps.machines.primary_machine(),
            controller: self.deps.machines.controller(),
            artifacts: self.deps.artifacts.clone(),
            interactions: sink.clone(),
            session_ops: sink,
            operation: self.deps.operations.with_id(
                self.meta.workspace.environment.clone(),
                operation,
                Origin::User {
                    session: Some(self.meta.id.clone()),
                },
            ),
            // 用户的命令不是模型这一步发的调用：没有对话摘录（auto-classifier.md §15.5）
            transcript: None,
        }
    }
}

fn user(id: rust_cli_kernel_api::ids::MessageId, text: String, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id,
        content: vec![ContentBlock::Text { text }],
        origin,
    })
}

/// 展开 `prompt` 消息的文字：不是命令时原样（一条人类输入，调模型）。
pub(crate) async fn expand(
    commands: &dyn UserCommands,
    ids: &dyn Ids,
    prompt: Message,
    context: ToolUseContext,
) -> Inputs {
    let Message::User(original) = &prompt else {
        return Inputs {
            messages: vec![prompt],
            query: true,
            plain: true,
        };
    };
    let text = original
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    match commands.expand(text, context).await {
        Expansion::NotCommand => Inputs {
            messages: vec![prompt.clone()],
            query: true,
            plain: true,
        },
        Expansion::Messages { messages, query } => {
            let mut prompt_id = Some(original.id.clone());
            let messages = messages
                .into_iter()
                .map(|expanded| match expanded.kind {
                    ExpandedKind::Input => {
                        let id = prompt_id.take().unwrap_or_else(|| ids.message());
                        user(id, expanded.text, UserOrigin::Human)
                    }
                    ExpandedKind::Meta { source } => {
                        user(ids.message(), expanded.text, UserOrigin::Meta { source })
                    }
                    // 例如 `-p` 下未知命令的提醒：随 attachment 规则在发给模型前上浮到输入之前。
                    ExpandedKind::Attachment { source } => Message::Attachment(AttachmentMessage {
                        id: ids.message(),
                        source,
                        text: expanded.text,
                        data: serde_json::Value::Null,
                    }),
                })
                .collect();
            Inputs {
                messages,
                query,
                plain: false,
            }
        }
    }
}

#[cfg(test)]
#[path = "slash_tests.rs"]
mod tests;
