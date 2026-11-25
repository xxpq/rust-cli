//! 管理命令与显示用的查询（docs/specs/rust-cli/tui-display.md §5，工作线 L28）。
//!
//! 后台任务的事实在执行环境（后台命令的账本）与 agents（后台子代理）里；这里只是给前端看的快照，
//! 前端在相关事件之后重取，不自己维护任务表。

use serde::{Deserialize, Serialize};

use crate::env_shell::BackgroundStatus;

/// 后台任务的种类（`local_bash` / `local_agent`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Shell,
    Agent,
    /// Monitor 起的后台命令（命令来源与 WebSocket 来源；修复线 F117 新增，docs/specs/rust-cli/tui-display.md §2.3）。
    Monitor,
}

/// 执行环境的后台命令的种类 → 会话的后台工作的种类（开回合的后台工作，docs/specs/rust-cli/session-lifecycle.md §10.50）。
impl From<crate::env_shell::BackgroundKind> for BackgroundKind {
    fn from(kind: crate::env_shell::BackgroundKind) -> Self {
        match kind {
            crate::env_shell::BackgroundKind::Bash => Self::Shell,
            crate::env_shell::BackgroundKind::Monitor => Self::Monitor,
        }
    }
}

/// 会话里的一个后台任务（`/tasks` 的一行、footer 的计数）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundEntry {
    /// 任务 ID：命令是 shell 任务 ID，子代理是 agentId（TaskStop 与 `Command::StopTask` 用它）。
    pub id: String,
    pub kind: BackgroundKind,
    /// 命令原文，或子代理的 description；monitor 是它的 description（修复线 F117，没有时是命令原文）。
    pub description: String,
    pub status: BackgroundStatus,
    /// 命令输出文件的末尾（最多 [`OUTPUT_TAIL_LINES`] 行）；子代理没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// 后台命令所在的机器；后台子代理是它的缺省机器（Agent 的 `machine`）；主 env 不写出（远端执行 R4，
    /// docs/specs/rust-cli/remote-exec.md §21.7）。
    #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
    pub machine: crate::ids::MachineName,
    /// 远端的后台命令所在的机器正在重连：这一条是那台机器上一份的内容（远端执行 F96，remote-exec.md §23.3）；本机与
    /// 连着的机器为 false（serde 缺省 false、不写出）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reconnecting: bool,
}

impl BackgroundEntry {
    /// 本机的一个后台任务，没有输出末尾（远端的机器与重连状态由会话 actor 用结构更新填上）。
    pub fn new(
        id: impl Into<String>,
        kind: BackgroundKind,
        description: impl Into<String>,
        status: BackgroundStatus,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            description: description.into(),
            status,
            output: None,
            machine: crate::ids::MachineName::default(),
            reconnecting: false,
        }
    }
}

/// `BackgroundEntry.output` 最多保留的行数。
pub const OUTPUT_TAIL_LINES: usize = 40;

/// MCP 服务器的连接状态（`/mcp` 与 `/status`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerState {
    Pending,
    Connected,
    Failed,
    /// 停用（配置里 `enabled: false`，或 `/mcp disable`，tui-manage.md §6）：不连接。
    Disabled,
    /// 远程服务器要（重新）授权（OAuth，docs/specs/rust-cli/ext.md §3.12.7）：不连接、不重连，等 Sign in。
    NeedsAuth,
}

/// 一个用 OAuth 的服务器有没有存下的令牌（ext.md §3.12.7）：界面据此给 Sign in，或 Sign in again 与 Sign out。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthState {
    SignedIn,
    SignedOut,
}

/// `/mcp` 的控制动作（docs/specs/rust-cli/tui-manage.md §6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAction {
    Reconnect,
    Enable,
    Disable,
}

/// 开始了的交互授权（`Kernel::mcp_sign_in`，ext.md §3.12.6）：地址交给浏览器；`done` 等回调、换令牌、重连，给出这个服务器
/// 之后的状态（这套内核没有它的连接时 None：会话级的服务器、没有打开的会话），失败时是给用户看的原因。授权由进程级的登记持有
/// （ext.md §3.12.14）：放下 `done` 不放弃授权；同一个授权目标再开始一次才丢下它（监听随之关掉）。
pub struct McpSignIn {
    pub url: String,
    pub done: futures_util::future::BoxFuture<'static, Result<Option<McpServerInfo>, String>>,
}

/// 一次交互授权的结局（ext.md §3.12.14；App 的 `mcp.oauth.signInOutcome`，18-mcp-trust.md §16.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpSignInOutcome {
    /// 换到了令牌、发布了，这个进程里要授权的连接已经重连。
    SignedIn,
    /// 没做成：给用户看的原因（回调里的拒绝、超时、换令牌失败）。
    Failed(String),
    /// 同一个授权目标又开始了一次，这一次被丢下。
    Replaced,
    /// 这一次已经不在进行（问得比它结束晚，或从没有过）。
    Ended,
}

/// 一个用 OAuth 的服务器的授权状态（App 的 `mcp.oauth.servers`，18-mcp-trust.md §16.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOAuthServer {
    /// 运行时的名字（插件的是 `plugin:<插件>:<服务器>`）。
    pub name: String,
    /// 存了令牌（只看键，不解密）。
    pub signed_in: bool,
    /// 这套内核里有一个连接要（重新）授权。
    pub needs_auth: bool,
    /// 进行中的授权的地址。
    pub pending_url: Option<String>,
}

impl std::fmt::Debug for McpSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSignIn")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// 对这些服务器做一个动作；回执是它们改动后的状态（reconnect 与 enable 等到离开 connecting）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpControl {
    pub action: McpAction,
    pub servers: Vec<String>,
}

/// 一个 MCP 服务器的状态快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub state: McpServerState,
    /// 连接失败的原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 已连接时提供的工具数。
    #[serde(default)]
    pub tools: usize,
    /// 服务器绑定的机器（R5，remote-exec.md §22.2）；主 env 不写出。
    #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
    pub machine: crate::ids::MachineName,
    /// 用 OAuth、且这个内核能授权时有没有存下的令牌（ext.md §3.12.7）；不用 OAuth 或不能授权时 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpAuthState>,
}

/// 一条已配置的 hook（`/hooks` 只读查看，docs/specs/rust-cli/tui-display.md §3.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEntry {
    /// 事件名（33 个 hook 事件名之一，例如 `PreToolUse`）。
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// `command`、`prompt`、`agent` 或 `http`。
    pub kind: String,
    /// 显示的文本：命令、提示词或地址。
    pub display: String,
    /// 来源：`user`、`project`、`local`、`policy`，插件是 `plugin:<名字>`。
    pub source: String,
}

/// `/cd <path>`（docs/specs/rust-cli/tui-manage.md §7）：`path` 是用户给的路径（相对会话 shell 的当前目录）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDirectory {
    pub workspace: crate::env::WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::ids::SessionId>,
    pub path: String,
}

/// `/cd` 成功：新的工作区根与显示路径（home 写成 `~`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryChanged {
    pub root: crate::env::EnvPath,
    pub display: String,
}

/// `Command::ChangeDirectory` 的结果（`CommandResult::Directory`，tui-manage.md §7.1，F1）：会话 actor 按自己的工作目录
/// 判定。给用户的文字由调用方按结果组成（显示路径要 home）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DirectoryOutcome {
    /// 换好了，说明已提交。
    Moved,
    /// 已经在这个目录（提示 `Already in <dir>.`）。
    Unchanged,
    /// 执行环境换不过去（目录不在了等）；`current` 是仍然在用的工作区根。
    Failed { current: crate::env::EnvPath },
}

/// `/add-dir <path>`（docs/specs/rust-cli/session-dirs.md §5.1）：`path` 是用户给的参数（相对工作区根）；还没有会话时 `drafts`
/// 是前端草稿里已经加的目录（有会话时判定在会话 actor，这一项不看）。`remember` 是确认框里选了「记住」（permission-updates.md
/// §6.1；检查时不看）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddDirectory {
    pub workspace: crate::env::WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::ids::SessionId>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drafts: Vec<crate::env::EnvPath>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remember: bool,
}

/// `/add-dir` 的目录能加（确认框之前的检查，permission-updates.md §6.1）：检查过的绝对路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryChecked {
    pub directory: crate::env::EnvPath,
}

/// `/add-dir` 加进去了：检查过的绝对路径与「记住」的结局。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryAdded {
    pub directory: crate::env::EnvPath,
    #[serde(default)]
    pub remembered: crate::working_dirs::Remembered,
}

/// `/plan open` 的计划文件查询（docs/specs/rust-cli/tui-manage.md §8）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFileQuery {
    pub workspace: crate::env::WorkspaceRef,
    pub session: crate::ids::SessionId,
}

/// 会话的计划文件（与 ExitPlanMode、计划模式 attachment 同一个位置）；`exists` 为真表示文件存在且不为空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFile {
    pub path: String,
    pub exists: bool,
}

/// `/export <文件名>` 的写入（docs/specs/rust-cli/tui-manage.md §3）：`text` 是前端渲染好的会话文字；`path`
/// 是用户给的路径（相对工作区根、`~/` 开头、或绝对路径；没有扩展名时内核补 `.txt`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFile {
    pub workspace: crate::env::WorkspaceRef,
    pub path: String,
    pub text: String,
}

#[cfg(test)]
#[path = "manage_tests.rs"]
mod tests;
