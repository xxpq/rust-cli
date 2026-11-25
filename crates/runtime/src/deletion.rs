//! 归档会话的永久删除（docs/specs/rust-cli/session-facts.md §5.1；02 §2.6）：预检只读库、列文件、问打开着的会话在不在用；执行在占住
//! 会话表的条目之后再预检一次，再写 Node 的删除凭据、删文件、删行。打开着的会话只由会话表管（占住、关闭），不另存状态。

use rust_cli_kernel_api::deletion::{
    BeginOutcome, DeletionBegin, DeletionBlock, DeletionOutcome, DeletionPreview, DeletionResult,
    DeletionScope, SessionFile, preview,
};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::{EndReason, KernelError, SessionHandle};

use super::*;

impl LocalKernel {
    /// 存储的范围（含凭据）、每个会话的文件与打开着的会话在不在用 → 预检（与执行同一个函数，`revision` 只随事实变）。`ask_open`：问树里打开着
    /// 的会话（不占条目；加载中、关闭中的等它换掉）；执行在占住条目之后不问（这时没有打开着的）。读不到视图（刚关掉）算不在用。
    async fn inspect(
        &self,
        root: &SessionId,
        workspace: &WorkspaceRef,
        ask_open: bool,
    ) -> Result<(DeletionScope, DeletionPreview), KernelError> {
        let scope = self.ports.store.deletion_scope(root).await?;
        let mut files: Vec<(SessionId, Vec<SessionFile>)> = Vec::new();
        let mut in_use = false;
        for session in &scope.sessions {
            files.push((
                session.clone(),
                self.ports.store.session_files(session).await?,
            ));
            if ask_open && let Some(open) = self.settled(session).await {
                in_use |= open.view().await.is_ok_and(|view| view.in_use());
            }
        }
        let preview = preview(workspace, &scope, &files, in_use);
        Ok((scope, preview))
    }

    pub(crate) async fn deletion_preview(
        &self,
        root: &SessionId,
        workspace: &WorkspaceRef,
    ) -> Result<DeletionPreview, KernelError> {
        Ok(self.inspect(root, workspace, true).await?.1)
    }

    /// session-facts.md §5.1 的 ①–⑦。`operation` 写进凭据的 `operation_id`。
    pub(crate) async fn delete_permanently(
        &self,
        root: &SessionId,
        workspace: &WorkspaceRef,
        revision: &str,
        operation: &str,
    ) -> Result<DeletionResult, KernelError> {
        let (_, first) = self.inspect(root, workspace, true).await?;
        let finished = |outcome: DeletionOutcome, preview: &DeletionPreview| DeletionResult {
            outcome,
            sessions: preview.sessions.clone(),
            retained: preview.retained.len(),
        };
        // 凭据全是 deleted：已经删完（Host 崩溃在删任务行之前、用户重试，同 Node）
        if first.completed {
            return Ok(finished(DeletionOutcome::Deleted, &first));
        }
        if let Some(block) = first.blocked {
            return Ok(finished(DeletionOutcome::Blocked(block), &first));
        }
        // 删到一半（凭据 deleting）时接着删，不再比确认时的 revision（同 Node）
        if !first.incomplete && first.revision != revision {
            return Ok(finished(
                DeletionOutcome::Blocked(DeletionBlock::Changed),
                &first,
            ));
        }
        // ② 按 id 排序逐个占住：同一进程里两次删除按同一个顺序占，不会互相等死
        let mut order = first.sessions.clone();
        order.sort();
        let mut held = Vec::with_capacity(order.len());
        for session in &order {
            loop {
                match self.claim(session).await {
                    crate::table::Claim::Open(open) => {
                        if open.view().await.is_ok_and(|view| view.in_use()) {
                            // 放开已占的（守卫随 held 丢掉）：在用的不停（PD-D3）
                            return Ok(finished(
                                DeletionOutcome::Blocked(DeletionBlock::Running),
                                &first,
                            ));
                        }
                        open.close(EndReason::Other).await;
                    }
                    crate::table::Claim::Load(loading) => {
                        held.push(loading);
                        break;
                    }
                }
            }
        }
        // ③ 占住之后再预检（关闭跑了 SessionEnd，可能写了记录）：这时没有打开着的会话
        let (scope, now) = self.inspect(root, workspace, false).await?;
        if let Some(block) = now.blocked {
            return Ok(finished(DeletionOutcome::Blocked(block), &now));
        }
        if !now.incomplete && now.revision != first.revision {
            return Ok(finished(
                DeletionOutcome::Blocked(DeletionBlock::Changed),
                &now,
            ));
        }
        // ④ 插凭据（Node `beginSessionDeletion`）：在关掉会话之后，0025 的触发器从这时起挡住写进这些会话；事务里再核对一次
        let begin = DeletionBegin {
            operation: operation.to_owned(),
            stamps: scope.stamps.clone(),
            retained: now
                .retained
                .iter()
                .map(|retained| (retained.session.clone(), retained.artifact.clone()))
                .collect(),
        };
        match self.ports.store.begin_deletion(root, &begin).await {
            Ok(BeginOutcome::Admitted) => {}
            Ok(BeginOutcome::Changed) => {
                return Ok(finished(
                    DeletionOutcome::Blocked(DeletionBlock::Changed),
                    &now,
                ));
            }
            Ok(BeginOutcome::Running) => {
                return Ok(finished(
                    DeletionOutcome::Blocked(DeletionBlock::Running),
                    &now,
                ));
            }
            // 凭据插不下：什么都没删
            Err(error) => return Err(KernelError::from(error)),
        }
        // ⑤ 先删文件：删不掉时凭据留着，重试接着删
        for session in &now.sessions {
            let keep = now.kept_files(session);
            if let Err(error) = self.ports.store.remove_session_files(session, &keep).await {
                return Ok(finished(
                    DeletionOutcome::Incomplete(error.to_string()),
                    &now,
                ));
            }
        }
        // ⑥ 一个事务删行、凭据改成 deleted；⑦ 放开条目（held 随作用域丢掉）：等着打开它的调用冷加载，看到 NotFound
        if let Err(error) = self.ports.store.complete_deletion(root).await {
            return Ok(finished(
                DeletionOutcome::Incomplete(error.to_string()),
                &now,
            ));
        }
        drop(held);
        tracing::info!(
            event = "session.permanent_delete",
            root = %root,
            sessions = now.sessions.len(),
            retained = now.retained.len(),
            "sessions permanently deleted"
        );
        Ok(finished(DeletionOutcome::Deleted, &now))
    }
}

#[cfg(test)]
#[path = "deletion_tests.rs"]
mod tests;
