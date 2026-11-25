//! rewind（docs/specs/rust-cli/session-lifecycle.md §5）：对话回退是一次存储提交；代码回退是
//! 执行环境的 token 事务，在后台工作里跑，结果回到 actor 后才提交事实、发事件、回复命令。

use std::sync::Arc;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::env::Origin;
use rust_cli_kernel_api::ids::{CommandId, MachineName, MessageId, OperationId};
use rust_cli_kernel_api::machine::{Checkpoints, not_connected};
use rust_cli_kernel_api::session_api::{
    Ack, CommandResult, KernelEvent, RewindOption, RewindScope, RewoundFile,
};
use rust_cli_kernel_api::store::{ArtifactStore, Fact, RevertCursor, WorkspaceCheckpoint};

use crate::actor::{Actor, Job, JobDone, rejected};
use crate::rewind_machines::{self, Group};
use crate::rewind_plan;

/// 选择器的快照：可选的 user 消息与每一回合的 checkpoint（读 artifact 在调用方做）。
pub(crate) struct Snapshot {
    options: Vec<(MessageId, Vec<WorkspaceCheckpoint>, bool)>,
}

/// 代码回退之后做什么：回执（`Rewind`），或接着开新输入的回合（`ReplaceInput`，spec §10.53）。
pub(crate) enum Then {
    Reply(crate::ledger::Reply),
    Replace(Box<crate::replace::Replacement>),
}

/// 代码回退事务的结果，回到 actor 后提交。
pub(crate) struct Finished {
    command: CommandId,
    target: MessageId,
    scope: RewindScope,
    then: Then,
    /// 每台机器的 token（提交之后释放）。
    tokens: Vec<(Arc<dyn Checkpoints>, OperationId)>,
    /// 撤销成功的 checkpoint（R3：跨机器时一台 apply 失败，成功的那几台也要记下）。
    restored: Vec<String>,
    result: Result<Vec<RewoundFile>, String>,
}

/// 每条可选消息这一回合的改动（列表每行的摘要）：与回合记录里的文件改动同一个函数（`turn_files::read`）；artifact 读不出的回合按没有改动显示。
pub(crate) async fn options(store: &dyn ArtifactStore, snapshot: Snapshot) -> Vec<RewindOption> {
    let mut out = Vec::new();
    for (message, turn, code) in snapshot.options {
        let files = crate::turn_files::read(store, &turn)
            .await
            .into_iter()
            .map(|file| file.change)
            .collect();
        out.push(RewindOption {
            message,
            files,
            code,
        });
    }
    out
}

/// 撤销过的 checkpoint 的开回合输入（视图与冷快照的 `reverted`，§10.54）：按撤销的顺序、不重复。
pub fn reverted_prompts(checkpoints: &[WorkspaceCheckpoint]) -> Vec<MessageId> {
    let mut prompts: Vec<MessageId> = Vec::new();
    for checkpoint in checkpoints {
        if !prompts.contains(&checkpoint.prompt) {
            prompts.push(checkpoint.prompt.clone());
        }
    }
    prompts
}

/// 回执里恢复的文件：各台机器的，按分组的顺序（本机在前，R3）。
fn rewound_files(groups: &[rewind_machines::Restored]) -> Vec<RewoundFile> {
    groups
        .iter()
        .flat_map(|(machine, files)| {
            files.iter().map(|file| RewoundFile {
                path: file.path.clone(),
                action: file.action,
                machine: machine.clone(),
            })
        })
        .collect()
}

impl Actor {
    pub(crate) fn rewind_snapshot(&self) -> Snapshot {
        let options = self
            .messages
            .iter()
            .filter_map(rewind_plan::selectable)
            .map(|user| {
                let turn = self
                    .checkpoints
                    .iter()
                    .filter(|c| c.prompt == user.id)
                    .cloned()
                    .collect();
                let code =
                    !rewind_plan::checkpoints_from(&self.messages, &self.checkpoints, &user.id)
                        .is_empty();
                (user.id.clone(), turn, code)
            })
            .collect();
        Snapshot { options }
    }

    pub(crate) async fn rewind(
        &mut self,
        command: CommandId,
        target: MessageId,
        scope: RewindScope,
        reply: crate::ledger::Reply,
    ) {
        if !self
            .messages
            .iter()
            .any(|m| m.id() == &target && rewind_plan::selectable(m).is_some())
        {
            self.commands.settle(
                reply,
                rejected(
                    Refusal::NotFound,
                    format!("{target} is not a message to rewind to"),
                ),
            );
            return;
        }
        let undo: Vec<WorkspaceCheckpoint> =
            rewind_plan::checkpoints_from(&self.messages, &self.checkpoints, &target)
                .into_iter()
                .cloned()
                .collect();
        let code = matches!(scope, RewindScope::Code | RewindScope::Both);
        if !code || (undo.is_empty() && scope == RewindScope::Both) {
            let ack = match self
                .rewind_conversation(Some(command), &target, Vec::new())
                .await
            {
                Ok(removed) => Ack::Accepted {
                    result: CommandResult::Rewound {
                        restored: Vec::new(),
                        removed,
                    },
                },
                Err(reason) => rejected(Refusal::Failed, reason),
            };
            self.commands.settle(reply, ack);
            return;
        }
        if undo.is_empty() {
            self.commands.settle(
                reply,
                rejected(Refusal::Unavailable, "there are no code changes to restore"),
            );
            return;
        }
        self.rewind_code(command, target, scope, undo, Then::Reply(reply));
    }

    /// 撤销的 checkpoint 已落库（`CheckpointsRestored`）：挪进 `reverted`（不能再回退，回合的 diff 照样从它们算），发 `FilesRestored`——
    /// 这些回合的文件改动是「已撤销」（§10.54，同 v4 的 `fileChanges.state = reverted`）。
    fn checkpoints_restored(&mut self, ids: &[String]) {
        let (undone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.checkpoints)
            .into_iter()
            .partition(|c| ids.contains(&c.id));
        self.checkpoints = kept;
        let prompts = reverted_prompts(&undone);
        self.reverted.extend(undone);
        self.log.append(KernelEvent::FilesRestored { prompts });
    }

    /// 回执或接着开回合（[`Then`]）。
    fn settle_then(&mut self, then: Then, ack: Ack) {
        match then {
            Then::Reply(reply) => self.commands.settle(reply, ack),
            Then::Replace(replacement) => self.commands.settle(replacement.reply, ack),
        }
    }

    /// 代码回退的事务（spec §5.2）：在后台工作里跑，结果回到 actor 后由 [`Self::finish_rewind`] 提交。
    pub(crate) fn rewind_code(
        &mut self,
        command: CommandId,
        target: MessageId,
        scope: RewindScope,
        undo: Vec<WorkspaceCheckpoint>,
        then: Then,
    ) {
        // 按机器分组（R3，remote-exec.md §20.7.5）：本机在前，远端按名字；远端没连上就整体拒绝。
        let groups = match self.rewind_groups(undo) {
            Ok(groups) => groups,
            // 远端没连上（R3）。
            Err(reason) => {
                self.settle_then(then, rejected(Refusal::Unavailable, reason));
                return;
            }
        };
        let tokens = groups
            .iter()
            .map(|group| (group.port.clone(), group.token().clone()))
            .collect();
        let store = self.deps.artifacts.clone();
        let task = self.deps.spawner.spawn("rewind-code", async move {
            let outcome = rewind_machines::restore(store.as_ref(), &groups).await;
            JobDone::Rewind(Box::new(Finished {
                command,
                target,
                scope,
                then,
                tokens,
                restored: outcome.restored,
                result: outcome.result.map(|done| rewound_files(&done)),
            }))
        });
        self.job = Some(Job { task, cancel: None });
    }

    /// 事务结束：先提交（撤销的 checkpoint、对话游标），再改内存、发事件、回复，最后释放 token。换掉输入时（[`Then::Replace`]）回复之前先开
    /// 新输入的回合。
    pub(crate) async fn finish_rewind(&mut self, finished: Finished) {
        let Finished {
            command,
            target,
            scope,
            then,
            tokens,
            restored: checkpoints,
            result,
        } = finished;
        let outcome = match result {
            // 跨机器时一台 apply 失败：成功的那几台已经改回，记下它们（不动对话），再拒绝（R3）。
            Err(reason) if !checkpoints.is_empty() => {
                let fact = Fact::CheckpointsRestored {
                    checkpoints: checkpoints.clone(),
                };
                if self.commit(Some(command), vec![fact]).await.is_ok() {
                    self.checkpoints_restored(&checkpoints);
                }
                Err(rejected(Refusal::Unavailable, reason))
            }
            // 代码改不回去（文件在本工程改过之后又被改了，或者执行环境报错）。
            Err(reason) => Err(rejected(Refusal::Unavailable, reason)),
            Ok(restored) => {
                let fact = Fact::CheckpointsRestored {
                    checkpoints: checkpoints.clone(),
                };
                // 编辑带文件（§10.55）：文件改回之后、截对话之前停被截掉的回合里起的后台工作（同一次处理）
                if matches!(&then, Then::Replace(replacement) if replacement.stop_cut_works) {
                    self.stop_cut_works(&target).await;
                }
                let committed = if scope == RewindScope::Both {
                    self.rewind_conversation(Some(command), &target, vec![fact])
                        .await
                } else {
                    self.commit(Some(command), vec![fact])
                        .await
                        .map(|()| 0)
                        .map_err(|error| error.to_string())
                };
                match committed {
                    Ok(removed) => {
                        self.checkpoints_restored(&checkpoints);
                        Ok((restored, removed))
                    }
                    Err(reason) => Err(rejected(Refusal::Failed, reason)),
                }
            }
        };
        let replace = match (then, outcome) {
            (Then::Replace(replacement), Ok((restored, removed))) => {
                Some((replacement, restored, removed))
            }
            (then, outcome) => {
                let ack = outcome.map_or_else(
                    |refused| refused,
                    |(restored, removed)| Ack::Accepted {
                        result: CommandResult::Rewound { restored, removed },
                    },
                );
                self.settle_then(then, ack);
                None
            }
        };
        drop(self.deps.spawner.spawn("rewind-finish", async move {
            for (port, token) in tokens {
                if let Err(error) = port.rewind_finish(&token).await {
                    tracing::warn!(%error, "rewind token was not released");
                }
            }
        }));
        match replace {
            Some((replacement, restored, removed)) => {
                self.start_replacement(*replacement, restored, removed)
                    .await;
            }
            None => self.start_next().await,
        }
    }

    /// esc 撤回刚发的输入（`Command::RestoreCancelled`，修复线 F38，spec §5.5）。判断与回退在
    /// actor 的一次处理里：TUI 判断时内核可能已经开了排队的回合或通知的回合，这里看到的才是唯一的事实。回执带回
    /// 这条输入（前端放回输入框；`HistoryRewritten` 之后的视图里已经没有它）。
    pub(crate) async fn restore_cancelled(&mut self, command: CommandId) -> Ack {
        if self.inbox.has_inputs() {
            return rejected(Refusal::Unavailable, "inputs are queued");
        }
        let Some(index) = self
            .messages
            .iter()
            .rposition(|m| rewind_plan::selectable(m).is_some())
        else {
            return rejected(Refusal::Unavailable, "there is no input to restore");
        };
        if rewind_plan::answered(&self.messages[index + 1..]) {
            return rejected(Refusal::Unavailable, "the input was answered");
        }
        let message = self.messages[index].clone();
        match self
            .rewind_conversation(Some(command), message.id(), Vec::new())
            .await
        {
            Ok(removed) => Ack::Accepted {
                result: CommandResult::Restored {
                    message: Box::new(message),
                    removed,
                },
            },
            Err(reason) => rejected(Refusal::Failed, reason),
        }
    }

    /// 要撤销的 checkpoint 按机器分组：本机在前，远端按名字；每组一个 token、那台机器的端口与工作区。
    fn rewind_groups(&self, undo: Vec<WorkspaceCheckpoint>) -> Result<Vec<Group>, String> {
        let mut names: Vec<MachineName> = Vec::new();
        for checkpoint in &undo {
            if !names.contains(&checkpoint.machine) {
                names.push(checkpoint.machine.clone());
            }
        }
        // 主 env 在前，额外机器按名字（`remote()` 的 None 排在前面）。
        names.sort_by(|a, b| a.remote().cmp(&b.remote()));
        let mut groups = Vec::new();
        for machine in names {
            let (port, workspace) = match machine.remote() {
                None => (
                    self.deps.machines.primary_machine().checkpoints().clone(),
                    self.meta.workspace.clone(),
                ),
                Some(name) => match self.deps.machines.remote(&machine) {
                    Some((env, workspace)) => (env.checkpoints().clone(), workspace),
                    None => {
                        return Err(format!("Cannot restore code: {}", not_connected(name)));
                    }
                },
            };
            let context = self.deps.operations.next(
                workspace.environment.clone(),
                Origin::User {
                    session: Some(self.meta.id.clone()),
                },
            );
            let checkpoints = undo
                .iter()
                .filter(|c| c.machine == machine)
                .cloned()
                .collect();
            groups.push(Group {
                machine,
                checkpoints,
                port,
                workspace,
                context,
            });
        }
        Ok(groups)
    }

    /// 历史截到 target 之前（`session.revert` 单游标）；`extra` 与游标在同一批提交。返回去掉的消息数。
    pub(crate) async fn rewind_conversation(
        &mut self,
        command: Option<CommandId>,
        target: &MessageId,
        extra: Vec<Fact>,
    ) -> Result<usize, String> {
        let index = self
            .messages
            .iter()
            .position(|m| m.id() == target)
            .ok_or_else(|| format!("{target} is not in the conversation"))?;
        let cut_after = self
            .messages
            .last()
            .map(|m| m.id().clone())
            .unwrap_or_else(|| target.clone());
        let generation = self.generation + 1;
        let cursor = RevertCursor {
            target: target.clone(),
            kept: self.messages[..index]
                .iter()
                .map(|m| m.id().clone())
                .collect(),
            cut_after,
            generation,
        };
        let mut facts = extra;
        facts.push(Fact::RevertSet { cursor });
        // 回执的拒绝原因是给前端的文字：存储的错误在这里按原文写进去。
        self.commit(command, facts)
            .await
            .map_err(|error| error.to_string())?;
        let removed = self.messages.len() - index;
        self.messages.truncate(index);
        self.generation = generation;
        self.log
            .append(KernelEvent::HistoryRewritten { generation });
        Ok(removed)
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "restore_cancelled_tests.rs"]
mod restore_cancelled_tests;
