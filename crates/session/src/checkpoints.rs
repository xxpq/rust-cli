//! 文件 checkpoint 进入会话清单（docs/specs/rust-cli/session-lifecycle.md §4）。
//!
//! 配对（`ToolFinished` 时记下、工具结果提交时同批落库，连同子代理关联）在 kernel-api 的 `RunLedger` 里，子代理的
//! sink 用同一份（工作线 F130）；这里只做提交成功之后的那一步。

use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::session_api::{FileChange, KernelEvent};
use rust_cli_kernel_api::store::WorkspaceCheckpoint;

use crate::actor::Actor;

impl Actor {
    /// 在跑的回合的 checkpoint（session-lifecycle.md §10.42）：回合任务读它们的 artifact，算这个回合的文件改动。与查询同一个取法
    /// （`turn_files::of_turn`，§10.45）。压缩的回合没有工具调用，清单为空。
    pub(crate) fn turn_checkpoints(&self) -> Vec<WorkspaceCheckpoint> {
        let Some(running) = &self.running else {
            return Vec::new();
        };
        crate::turn_files::of_turn(
            self.earlier.iter().chain(&self.messages),
            &self.turns,
            &self.checkpoints,
            &running.turn,
        )
    }

    /// 一个回合的文件 diff 的快照（`Request::TurnFiles`，§10.45）：记录与 checkpoint，一次处理，不做 IO。撤销过的回合照样给 diff（同 v4，§10.54）：
    /// 它的 checkpoint 在 `reverted` 里。
    pub(crate) fn turn_files(&self, turn: &TurnId) -> crate::turn_files::TurnFiles {
        crate::turn_files::TurnFiles::of(
            self.earlier.iter().chain(&self.messages),
            &self.turns,
            &[self.checkpoints.as_slice(), self.reverted.as_slice()].concat(),
            turn,
        )
    }

    /// 提交成功后：进入清单，发 `FileChanges`。
    pub(crate) fn checkpoints_settled(&mut self, settled: Vec<(WorkspaceCheckpoint, FileChange)>) {
        let Some(turn) = self.running.as_ref().map(|r| r.turn.clone()) else {
            return;
        };
        if settled.is_empty() {
            return;
        }
        let mut files = Vec::new();
        for (checkpoint, change) in settled {
            self.checkpoints.push(checkpoint);
            files.push(change);
        }
        self.log.append(KernelEvent::FileChanges {
            turn,
            files,
            rewindable: true,
        });
    }
}
