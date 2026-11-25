//! 回退文件的预览（docs/specs/rust-cli/session-lifecycle.md §10.53）：`Rewind{target, Code}` 会恢复的文件与现在能不能恢复。范围与回退相同
//! （[`rewind_plan::checkpoints_from`]），模拟与回退的计划相同（[`rewind_plan::walk`]），现在的内容经那台机器的文件端口读、按执行环境核对用的
//! 同一个 hash 算。只读：不登记 token，不写。打开着的会话由 actor 给出 checkpoint（一次往返），读 artifact 与文件在调用方的任务里；没打开的由
//! runtime 从存储冷加载后调 [`rewind_preview`]。

use rust_cli_kernel_api::env::{EnvError, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::env_rewind::{RestoreAction, content_hash};
use rust_cli_kernel_api::ids::{MachineName, MessageId};
use rust_cli_kernel_api::machine::not_connected;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::rewind_preview::{
    PreviewFile, PreviewState, RewindPreview, UnreadableCheckpoint,
};
use rust_cli_kernel_api::store::{ArtifactStore, WorkspaceCheckpoint};

use crate::actor::Actor;
use crate::rewind_plan::{self, Artifact, Fault, Walked};

/// 回退会撤销的 checkpoint 与会话的工作区（actor 给出的快照）。
pub(crate) struct Undo {
    pub(crate) checkpoints: Vec<WorkspaceCheckpoint>,
    pub(crate) workspace: WorkspaceRef,
}

/// target 起要撤销的 checkpoint；target 不是可选的输入时为 None（同 `Rewind` 的判断）。
fn undo_from(
    messages: &[Message],
    checkpoints: &[WorkspaceCheckpoint],
    target: &MessageId,
) -> Option<Vec<WorkspaceCheckpoint>> {
    messages
        .iter()
        .any(|m| m.id() == target && rewind_plan::selectable(m).is_some())
        .then(|| {
            rewind_plan::checkpoints_from(messages, checkpoints, target)
                .into_iter()
                .cloned()
                .collect()
        })
}

impl Actor {
    pub(crate) fn rewind_undo(&self, target: &MessageId) -> Option<Undo> {
        Some(Undo {
            checkpoints: undo_from(&self.messages, &self.checkpoints, target)?,
            workspace: self.meta.workspace.clone(),
        })
    }
}

/// 没打开的会话（runtime 冷加载之后）：`messages` 与 `checkpoints` 是存储给的，同会话打开之后 actor 的起始状态。
pub async fn rewind_preview(
    artifacts: &dyn ArtifactStore,
    machines: &dyn EnvironmentRegistry,
    workspace: &WorkspaceRef,
    messages: &[Message],
    checkpoints: &[WorkspaceCheckpoint],
    target: &MessageId,
) -> Option<RewindPreview> {
    let undo = undo_from(messages, checkpoints, target)?;
    Some(preview(artifacts, machines, workspace, undo).await)
}

async fn read_artifact(
    store: &dyn ArtifactStore,
    checkpoint: &WorkspaceCheckpoint,
) -> Result<Artifact, String> {
    let bytes = store
        .get(&checkpoint.snapshot)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// 预览：按机器分组（主 env 在前、远端按名字，同回退的事务），组内按路径。
pub(crate) async fn preview(
    artifacts: &dyn ArtifactStore,
    machines: &dyn EnvironmentRegistry,
    workspace: &WorkspaceRef,
    undo: Vec<WorkspaceCheckpoint>,
) -> RewindPreview {
    let mut names: Vec<MachineName> = Vec::new();
    for checkpoint in &undo {
        if !names.contains(&checkpoint.machine) {
            names.push(checkpoint.machine.clone());
        }
    }
    names.sort_by(|a, b| a.remote().cmp(&b.remote()));
    let mut out = RewindPreview::default();
    for machine in names {
        let mut read = Vec::new();
        for checkpoint in undo.iter().filter(|c| c.machine == machine) {
            match read_artifact(artifacts, checkpoint).await {
                Ok(artifact) => read.push(artifact),
                Err(message) => out.unreadable.push(UnreadableCheckpoint {
                    checkpoint: checkpoint.id.clone(),
                    message,
                }),
            }
        }
        let (mut walked, _) = rewind_plan::walk(&read);
        walked.sort_by(|a, b| a.path.cmp(&b.path));
        for walked in walked {
            let state = state(machines, workspace, &machine, &walked).await;
            out.files.push(PreviewFile {
                path: walked.path,
                machine: machine.clone(),
                action: if walked.delete {
                    RestoreAction::Delete
                } else {
                    RestoreAction::Restore
                },
                writes: walked.writes,
                tools: walked.tools,
                state,
            });
        }
    }
    out
}

/// 一个文件现在能不能恢复：模拟出错的照它；否则读磁盘上现在的内容，与最后一次写入之后的比（同执行环境的核对：不存在为 None）。
async fn state(
    machines: &dyn EnvironmentRegistry,
    workspace: &WorkspaceRef,
    machine: &MachineName,
    walked: &Walked,
) -> PreviewState {
    match &walked.fault {
        Some(Fault::NoContent) => return PreviewState::NoContent,
        Some(Fault::Gap { expected, current }) => {
            return PreviewState::Changed {
                expected: expected.clone(),
                current: current.clone(),
            };
        }
        None => {}
    }
    let (env, workspace) = match machine.remote() {
        None => (machines.primary_machine(), workspace.clone()),
        Some(name) => match machines.remote(machine) {
            Some(found) => found,
            None => {
                return PreviewState::Unreadable {
                    message: not_connected(name),
                };
            }
        },
    };
    let request = ReadFile {
        path: rust_cli_kernel_api::env::EnvPath::new(&walked.path),
    };
    let current = match env.files().read_file(&workspace, request).await {
        Ok(file) => Some(content_hash(&file.bytes)),
        Err(EnvError::NotFound { .. }) => None,
        Err(error) => {
            return PreviewState::Unreadable {
                message: error.to_string(),
            };
        }
    };
    if current == walked.expected {
        PreviewState::Ready
    } else {
        PreviewState::Changed {
            expected: walked.expected.clone(),
            current,
        }
    }
}

#[cfg(test)]
#[path = "rewind_preview_tests.rs"]
mod tests;
