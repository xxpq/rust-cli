//! ListAgents、SendMessage（docs/specs/rust-cli/agents.md §6）：本进程里启动过的子代理与本机的同伴会话
//! （docs/specs/rust-cli/cross-session.md，F28；段落与同伴寻址在 `peer_send`）。描述与 schema 在 `prompts/tools/`
//! （ListAgents、SendMessage），删掉了不实的产品句（cross-session.md §2.7）。TaskStop 在 `task_stop`。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::classifier::ClassifierInput;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, parse,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::Core;
use crate::live::Delivery;
use crate::spawn;
use crate::task_stop::TaskStopTool;
use rust_cli_kernel_api::tool_names;

pub(crate) fn tools(core: &Arc<Core>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = [Kind::ListAgents, Kind::SendMessage]
        .into_iter()
        .map(|kind| {
            Arc::new(Messaging {
                core: core.clone(),
                kind,
            }) as Arc<dyn Tool>
        })
        .collect();
    tools.push(Arc::new(TaskStopTool { core: core.clone() }));
    tools
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ListAgents,
    SendMessage,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::ListAgents => tool_names::LIST_AGENTS,
            Self::SendMessage => tool_names::SEND_MESSAGE,
        }
    }
}

struct Messaging {
    core: Arc<Core>,
    kind: Kind,
}

#[derive(Deserialize)]
struct SendInput {
    to: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    message: String,
}

impl Messaging {
    /// 段落格式：自己那一段、子代理、同伴会话（cross-session.md §1.3）。
    async fn list(&self, context: &ToolUseContext) -> ToolOutput {
        rust_cli_kernel_api::tool::text(crate::peer_send::listing(&self.core, context).await)
    }

    async fn send(
        &self,
        input: SendInput,
        context: &ToolUseContext,
    ) -> Result<ToolOutput, ToolError> {
        let id = AgentId::new(input.to.trim());
        let delivery = self.core.live.deliver(
            &id,
            &context.session,
            input.message.clone(),
            context.tool_use.clone(),
        );
        let Some(delivery) = delivery else {
            // 进程内没有：按本机的同伴会话寻址（cross-session.md §1.4）。都没有时是打开跨会话后的
            // `No agent named '<to>' is reachable.`（结果 `success: false`、不标 is_error）。以前是关闭跨会话时的
            // `… is currently addressable …`，并标 is_error。
            let text = crate::peer_send::send(
                &self.core,
                &input.to,
                input.summary.as_deref(),
                &input.message,
                context,
            )
            .await;
            return Ok(rust_cli_kernel_api::tool::text(text));
        };
        let (status, agent) = match delivery {
            Delivery::Queued => {
                let text = format!(
                    "Message queued for delivery to {} at its next tool round.",
                    input.to
                );
                return Ok(rust_cli_kernel_api::tool::text(text));
            }
            // 用户停过的不续跑，模型拿到对照实现的那一句（`success: false`、不标 is_error，agents.md §6）。修复：以前用的是
            // 另一句（`Agent "<to>" was stopped by the user and was not resumed. …`），用户停掉、通知送达之后再发消息时
            // 对照实现回的是这一句。
            Delivery::StoppedByUser => {
                let text = format!(
                    "Agent {} was stopped by the user and won't be resumed. Treat its work as cancelled; only launch a new agent if the user explicitly asks.",
                    input.to
                );
                return Ok(rust_cli_kernel_api::tool::text(text));
            }
            // 停了、还没退出：模型拿到对照实现的说明（`success: false`、不标 is_error，agents.md §6）。状态写收尾之后的终态 killed；收尾之后再发就是续跑
            Delivery::StillStopping => {
                let text = format!(
                    "Agent \"{to}\" is stopped ({status}) and could not be resumed: Agent {id} is still stopping \u{2014} its previous run was stopped but has not exited. Re-run {task_stop} on it or wait for it to exit before resuming.",
                    to = input.to,
                    status = BackgroundStatus::Killed.as_str(),
                    id = id.as_str(),
                    task_stop = tool_names::TASK_STOP,
                );
                return Ok(rust_cli_kernel_api::tool::text(text));
            }
            Delivery::Restarted { previous, agent } => (previous.as_str(), *agent),
        };
        // 已结束：在后台续跑子会话。这次运行属于这次 SendMessage 调用（之后的通知指向它，agents.md §6）。
        let origin = rust_cli_kernel_api::message::UserOrigin::Human;
        let opening = match spawn::resume(&self.core, &agent, &input.message, origin).await {
            Ok(opening) => opening,
            Err(error) => {
                self.core.live.finish(&id, crate::report::Ending::Failed);
                return Err(ToolError::Failed {
                    message: error.to_string(),
                });
            }
        };
        // 续跑的子代理重新是一项后台工作（K-W1，session-lifecycle.md §10.28）
        let launch = crate::views::background_launch(&agent);
        spawn::background(&self.core, agent, opening);
        let text = format!(
            "Agent \"{}\" was stopped ({status}); it was resumed in the background with your message. You'll be notified when it finishes.",
            input.to
        );
        Ok(launch.attach(rust_cli_kernel_api::tool::text(text)))
    }
}

#[async_trait]
impl Tool for Messaging {
    fn classifier_input(&self, input: &Value) -> ClassifierInput {
        match self.kind {
            Kind::SendMessage => crate::classifier_input::send_message(input),
            // 没有要判的内容（同对照实现的缺省，auto-classifier.md §15.3）
            Kind::ListAgents => ClassifierInput::Skip,
        }
    }

    fn name(&self) -> &str {
        self.kind.name()
    }

    /// 延迟加载（docs/specs/rust-cli/tools-more.md §3.1）：ListAgents 内联，SendMessage 延迟。
    fn should_defer(&self) -> bool {
        self.kind == Kind::SendMessage
    }

    /// ToolSearch 的关键词提示。
    fn search_hint(&self) -> Option<&str> {
        Some(match self.kind {
            Kind::ListAgents => "list agents you can SendMessage to",
            Kind::SendMessage => "send messages to agent teammates",
        })
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::definition(self.kind.name())
            .schema
            .clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::definition(self.kind.name())
            .description
            .clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        self.kind == Kind::ListAgents
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        self.kind == Kind::ListAgents
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        value: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        match self.kind {
            Kind::ListAgents => Ok(self.list(&context).await),
            Kind::SendMessage => {
                let input = parse(&value)?;
                self.send(input, &context).await
            }
        }
    }
}
