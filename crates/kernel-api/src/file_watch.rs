//! 文件变化的订阅（docs/specs/rust-cli/ext.md §2.1.1）：进程级的全局 watcher 统一监听、按订阅分发。这里只是接口，实现在
//! 另一条工作线；没有实现（或某个订阅监听不了）时，调用方照旧每一步自己 stat。

use futures_util::stream::BoxStream;

use crate::env::{EnvPath, WorkspaceRef};

/// 一次订阅：这些目录与它们下面的全部层级。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRequest {
    /// 可以还不存在：之后建出来、删掉，都算一次变化。
    pub dirs: Vec<EnvPath>,
}

pub trait FileWatch: Send + Sync {
    /// 订阅 `workspace` 所在环境上的这些目录。流的每一项只是“重读一次”的提醒（可以合并，不带内容）；登记完成时先产出一项，
    /// 订阅方据此重读一次，补上登记期间的变化。流结束表示订阅失效（订阅方回到每一步 stat）。监听不了（远端机器、不支持）时
    /// 返回 None。放下流就是退订。
    fn subscribe(
        &self,
        workspace: &WorkspaceRef,
        request: WatchRequest,
    ) -> Option<BoxStream<'static, ()>>;
}
