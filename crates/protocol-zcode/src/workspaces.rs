//! 进程里的工作区（docs/specs/rust-cli/app-protocol.md §15，工作线 F145）：App 里一个进程服务所有本地工作区，每个
//! 工作区一套内核。入口（cli 的 App 组装）实现 [`Workspaces`]；协议层只按 Host 给的工作区路径要它，会话归哪个工作区
//! 记在 [`Context`](crate::Context) 的路由表里。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::Kernel;

/// 一个工作区的内核与新会话的缺省模式（本机项目的持久模式 → 配置 → default，按这个工作区）。
#[derive(Clone)]
pub struct Hosted {
    pub kernel: Arc<dyn Kernel>,
    pub mode: PermissionMode,
}

/// 入口按工作区组装的内核。
#[async_trait]
pub trait Workspaces: Send + Sync {
    /// 工作区 `root`（Host 的 `workspacePath`）的那一套：第一次见到时组装，之后一直是这一份；同一个工作区并发的组装
    /// 只做一次。
    async fn open(&self, root: &str) -> Result<Hosted, String>;

    /// 已经组装过的那一套（不组装）。
    fn existing(&self, root: &str) -> Option<Hosted>;

    /// 进程 cwd 的那一套（启动时组装）：没有登记的会话的只读请求用它，只读会话库，不用它的配置跑会话。
    fn process(&self) -> Hosted;
}
