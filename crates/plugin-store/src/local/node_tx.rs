//! 收尾 Node 留下的、owner 已死的事务（plugin-store.md §7；Node `atomic-directory.ts` 的 `recoverAtomicTargetSync`）。
//!
//! 只在持存储锁时、改写某个记录或替换某个目录之前，对相关的目标调用。为什么：rust 改写这条记录之后 `cacheTransactionId`
//! 就没了，Node 以后再恢复会判成「没提交」，删掉目标、把备份改回来（对市场目录就是倒退）。读者不调（F11）。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::diagnostic::StoreError;
use crate::core::records::contains_transaction;

/// Node 事务文件里的字段与取值。
const COORDINATED: &str = "coordinated";
const VERSION_V2: u64 = 2;

fn sibling(target: &Path, suffix: &str) -> Option<PathBuf> {
    let name = target.file_name()?.to_string_lossy();
    Some(target.parent()?.join(format!(".{name}{suffix}")))
}

/// Node 的事务文件（v2 的字段；v1 只有 `stageName`）。
struct Transaction {
    v2: bool,
    coordinated: bool,
    authority: Option<PathBuf>,
    had_target: bool,
    owner_pid: Option<u32>,
    stage: Option<String>,
    id: Option<String>,
}

fn parse(value: &Value) -> Option<Transaction> {
    let object = value.as_object()?;
    let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
    let version = object.get("version").and_then(Value::as_u64)?;
    Some(Transaction {
        v2: version == VERSION_V2,
        coordinated: text("mode").as_deref() == Some(COORDINATED),
        authority: text("authorityPath").map(PathBuf::from),
        had_target: object
            .get("hadTarget")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        owner_pid: object
            .get("ownerPid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        stage: text("stageName"),
        id: text("transactionId"),
    })
}

async fn exists(path: &Path) -> bool {
    rust_cli_fs::is_dir(path).await || rust_cli_fs::is_file(path).await
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> StoreError + '_ {
    move |error| StoreError::io(path, &error)
}

/// 收尾 `target` 上的事务；没有事务文件也没有备份时什么都不做。
pub(crate) async fn settle(target: &Path) -> Result<(), StoreError> {
    let (Some(transaction_path), Some(backup)) = (
        sibling(target, ".transaction.json"),
        sibling(target, ".backup"),
    ) else {
        return Ok(());
    };
    let transaction = rust_cli_fs::read_optional(&transaction_path)
        .await
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| parse(&value));
    let has_backup = exists(&backup).await;
    if transaction.is_none() && !has_backup {
        return Ok(());
    }
    if let Some(transaction) = &transaction
        && transaction.v2
        && let Some(pid) = transaction.owner_pid
        && pid != std::process::id()
        && rust_cli_fs::process_alive(pid)
    {
        // 活着的写者（不在存储锁里的播种）：不抢它的备份与暂存。
        return Ok(());
    }
    let has_target = exists(target).await;
    match &transaction {
        Some(tx) if tx.v2 && tx.coordinated => {
            let committed = match (&tx.authority, &tx.id) {
                (Some(authority), Some(id)) => rust_cli_fs::read_optional(authority)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|value| contains_transaction(&value, id)),
                _ => false,
            };
            if committed {
                if has_target && has_backup {
                    rust_cli_fs::remove_tree(&backup)
                        .await
                        .map_err(io(&backup))?;
                } else if !has_target && has_backup {
                    rust_cli_fs::rename(&backup, target)
                        .await
                        .map_err(io(target))?;
                }
            } else if has_backup {
                rust_cli_fs::remove_tree(target).await.map_err(io(target))?;
                rust_cli_fs::rename(&backup, target)
                    .await
                    .map_err(io(target))?;
            } else if !tx.had_target {
                rust_cli_fs::remove_tree(target).await.map_err(io(target))?;
            }
        }
        _ => {
            if !has_target && has_backup {
                rust_cli_fs::rename(&backup, target)
                    .await
                    .map_err(io(target))?;
            } else if has_target && has_backup {
                rust_cli_fs::remove_tree(&backup)
                    .await
                    .map_err(io(&backup))?;
            }
        }
    }
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let (Some(stage), Some(parent)) = (
        transaction.as_ref().and_then(|tx| tx.stage.as_ref()),
        target.parent(),
    ) && stage.starts_with(&format!(".{name}.stage-"))
    {
        rust_cli_fs::remove_tree(&parent.join(stage))
            .await
            .map_err(io(target))?;
    }
    rust_cli_fs::remove_optional(&transaction_path)
        .await
        .map_err(io(&transaction_path))?;
    tracing::info!(event = "plugins.node_transaction.settled", target = %target.display(), "settled a dead Node transaction");
    Ok(())
}

#[cfg(test)]
#[path = "node_tx_tests.rs"]
mod tests;
