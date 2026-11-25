//! 每个 hook 的运行事实（docs/specs/rust-cli/hooks.md §14、session-lifecycle.md §10.51）：引擎交出这次事件里真的跑了的 hook
//! （[`HookRun`]），调用点经 [`SessionOps::hooks_ran`] 交给会话的 owner，会话 actor 把它挂在回合上（[`TurnHooks`]）。
//! 前端无关：只说哪个 hook、从哪来、怎么结束、为什么、什么时刻；行怎么画是前端的事。文字在引擎里已脱敏。

use serde::{Deserialize, Serialize};

use crate::hooks::{HookEventName, HookOutcome};
use crate::ids::{MessageId, ToolUseId, TurnId};
use crate::session_ops::SessionOps;

/// 一个 hook 来自哪里：设置快照的来源层或插件（hooks.md §2.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HookOrigin {
    User,
    Project,
    /// `settings.json` 形状的设置的两层（Node 的配置没有这两层）。
    Local,
    Policy,
    /// 插件的 hooks（`PluginHooks.plugin`）。
    Plugin {
        name: String,
    },
}

/// 一个 hook 一次运行的结局（hooks.md §14.2 的表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookRunOutcome {
    Success,
    Blocked,
    Failed,
    Cancelled,
    TimedOut,
}

/// 一个 hook 的一次运行：引擎在这次事件的 hook 都结束之后交出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRun {
    /// 在这次事件选出的 hook 里的位置（配置的顺序，去重之后）。
    pub index: usize,
    pub origin: HookOrigin,
    /// 显示用的命令（已脱敏）。
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    pub outcome: HookRunOutcome,
    /// 拦下时的原因（已脱敏，最多 4000 字符）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 拦下时 hook 的 stderr 预览（已脱敏，最多 4000 字符）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// 引擎的时钟（Unix 毫秒）：这个 hook 开始执行与结束时各读一次。
    pub started_at: u64,
    pub ended_at: u64,
}

/// 工具事件的那次调用（PreToolUse、PermissionRequest、PostToolUse、PostToolUseFailure）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookTool {
    pub tool_use: ToolUseId,
    pub name: String,
}

/// 一次 hook 事件里真的跑了的 hook。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookInvocation {
    pub event: HookEventName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<HookTool>,
    pub runs: Vec<HookRun>,
}

/// UserPromptSubmit 拦下了开回合的输入（这条输入不落库）：它的消息 id（没有落库）与原文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedPrompt {
    pub message: MessageId,
    pub text: String,
}

/// 会话 actor 记下的一次：挂在哪个回合上、这一轮里第几次（从 0 起）。只在内存（session-lifecycle.md §10.51）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnHooks {
    pub turn: TurnId,
    pub seq: usize,
    pub invocation: HookInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<BlockedPrompt>,
}

/// 把这次事件里真的跑了的 hook 交给会话的 owner（hooks.md §14.3）；没有跑 hook 时不交。
pub async fn report(
    ops: &dyn SessionOps,
    event: HookEventName,
    tool: Option<HookTool>,
    outcome: &HookOutcome,
) {
    if outcome.runs.is_empty() {
        return;
    }
    ops.hooks_ran(HookInvocation {
        event,
        tool,
        runs: outcome.runs.clone(),
    })
    .await;
}
