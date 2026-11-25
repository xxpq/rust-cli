//! 状态栏第 2 行的分支与行数（docs/specs/rust-cli/tui-display.md §2.6「第 2 行」）：问内核的 `working_tree`，回执按序号写进
//! `App.working_tree`（`ui/status_place.rs`）。界面不另算，也不定时轮询。
use futures_util::FutureExt;
use rust_cli_kernel_api::env_git::WorkingTree;
use rust_cli_kernel_api::session_api::KernelError;

use super::Loop;
use super::session::Done;

/// 工作树的回执：序号、问的时候的工作区根与内核的回答。
pub(super) struct TreeDone {
    seq: u64,
    root: String,
    tree: Result<WorkingTree, KernelError>,
}

impl Loop<'_> {
    /// 现在的工作区还没问过时问一次。启动、切换会话、`/cd`、EnterWorktree、草稿换文件夹都只是根变了，都走到这里（每轮主循环
    /// 比一次根），不在每个换目录的地方各问一次。
    pub(super) fn sync_working_tree(&mut self) {
        if self.app.working_tree.stale(self.app.workspace()) {
            self.refresh_working_tree();
        }
    }

    /// 再问一次：回合结束、`!` 命令结束（可能改了文件）、终端得到焦点（别处可能改了）。流式中不问。
    pub(super) fn refresh_working_tree(&mut self) {
        let workspace = self.app.view.workspace().clone();
        let root = workspace.root.as_str().to_owned();
        let seq = self.app.working_tree.begin(&root);
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let tree = kernel.working_tree(&workspace).await;
                Done::WorkingTree(TreeDone { seq, root, tree })
            }
            .boxed(),
        );
    }

    pub(super) fn on_working_tree(&mut self, done: TreeDone) {
        let tree = match done.tree {
            Ok(tree) => Some(tree),
            // 内核不支持（测试替身、别的入口）：只画目录。
            Err(KernelError::Unsupported { .. }) => None,
            Err(error) => {
                tracing::warn!("working tree: {error}");
                None
            }
        };
        self.app.working_tree.arrived(done.seq, done.root, tree);
    }
}
