//! 一组权限更新的 owner（docs/specs/rust-cli/permission-updates.md §4）：用户的「总是允许」、PermissionRequest hook 的
//! `updatedPermissions`（随回答，或没有人能回答时管线另请）、前端的 `Command::UpdatePermissions` 都经 [`Actor::apply_updates`]。
//!
//! 先算后提交：会话层、模式与目录的新值和要提交的事实一次算出；一批提交成功之后才改内存、发事件，失败时一项都不改。会话层与目录
//! 只有这里与 `dirs.rs` 写（附加目录的 owner 是会话 actor）。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::permission::{ExecutionState, SourcedRule};
use rust_cli_kernel_api::permission_updates::{
    PermissionUpdate, UpdatePlace, absolute_only, apply_to_layer, local_facts,
};
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::store::{Fact, StoreError};
use rust_cli_kernel_api::working_dirs;

use crate::actor::Actor;

/// 一组更新算出的结果：新的会话层、执行状态与附加目录，要提交的事实。
struct Planned {
    layer: Vec<SourcedRule>,
    execution: ExecutionState,
    directories: Vec<EnvPath>,
    facts: Vec<Fact>,
}

impl Actor {
    /// 套用一组权限更新：一批提交，成功后改会话层、模式与目录并发事件（模式的收尾与前端切模式相同）。`command` 是前端命令的 id
    /// （hook 的更新没有）。
    pub(crate) async fn apply_updates(
        &mut self,
        command: Option<CommandId>,
        updates: &[PermissionUpdate],
    ) -> Result<(), StoreError> {
        if updates.is_empty() {
            return Ok(());
        }
        let updates = absolute_only(updates, &self.meta.workspace.root);
        let planned = self.plan_updates(&updates);
        if !planned.facts.is_empty() {
            self.commit(command, planned.facts).await?;
        }
        self.session_rules = planned.layer;
        let before = std::mem::replace(&mut self.additional_directories, planned.directories);
        for directory in before
            .iter()
            .filter(|dir| !self.additional_directories.contains(dir))
        {
            self.log.append(KernelEvent::DirectoryRemoved {
                directory: directory.clone(),
            });
        }
        for directory in self
            .additional_directories
            .iter()
            .filter(|dir| !before.contains(dir))
        {
            self.log.append(KernelEvent::DirectoryAdded {
                directory: directory.clone(),
            });
        }
        if planned.execution != self.execution {
            // 同工具切 plan：回合在跑时放行的同伴消息排到回合之后，空闲时可以现在开（peers_mode_changed 自己判）。
            self.execution_changed(planned.execution, true, None).await;
        }
        tracing::info!(
            event = "permissions.updated",
            session = %self.meta.id,
            updates = updates.len(),
            session_rules = self.session_rules.len(),
            "permission updates applied"
        );
        Ok(())
    }

    /// 纯：按顺序套用，不改状态。目录已去掉相对路径（`absolute_only`），已在工作目录里的不加（同 `/add-dir`）；模式最后一个生效，
    /// 有落在本机的 `SetMode` 时另记项目的持久模式。
    fn plan_updates(&self, updates: &[PermissionUpdate]) -> Planned {
        let root = &self.meta.workspace.root;
        let mut layer = self.session_rules.clone();
        let mut execution = self.execution;
        let mut directories = self.additional_directories.clone();
        let mut save_mode = false;
        for update in updates {
            apply_to_layer(&mut layer, update);
            match update {
                PermissionUpdate::SetMode { place, mode } => {
                    execution = execution.with_mode(*mode);
                    save_mode |= *place == UpdatePlace::Local;
                }
                PermissionUpdate::AddDirectories {
                    directories: added, ..
                } => {
                    for dir in added {
                        if working_dirs::covering(root, &directories, dir.as_str()).is_none() {
                            directories.push(dir.clone());
                        }
                    }
                }
                PermissionUpdate::RemoveDirectories {
                    directories: removed,
                    ..
                } => directories.retain(|dir| !removed.contains(dir)),
                PermissionUpdate::AddRules { .. }
                | PermissionUpdate::ReplaceRules { .. }
                | PermissionUpdate::RemoveRules { .. } => {}
            }
        }
        let mut facts = local_facts(updates);
        if execution != self.execution {
            facts.push(Fact::ExecutionChanged { state: execution });
        }
        if save_mode {
            facts.push(Fact::ProjectModeSet {
                mode: execution.mode,
            });
        }
        Planned {
            layer,
            execution,
            directories,
            facts,
        }
    }
}

#[cfg(test)]
#[path = "permission_updates_tests.rs"]
mod tests;
