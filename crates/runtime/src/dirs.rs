//! `/add-dir <path>`（docs/specs/rust-cli/session-dirs.md §5.1、permission-updates.md §6.1）：这里解析与检查路径（IO 不进会话
//! actor）。确认框之前先检查（[`check`]：路径与是否已在工作目录里，不加）；选了之后再检查一次再加（[`add`]）：有会话时交给会话
//! actor 判定与追加（`Command::AddDirectory`，附加目录的唯一 owner 是 actor，「记住」也在那里落）；没有会话时按前端的草稿判定，
//! 「记住」经 `LocalSettings::remember_directories` 直接存。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::manage::{AddDirectory, DirectoryAdded, DirectoryChecked};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, Kernel, KernelError,
};
use rust_cli_kernel_api::user_command::{LocalCommandOutput, LocalCommandRecord, bold};
use rust_cli_kernel_api::working_dirs::{self, Addition, Remembered};

use crate::LocalKernel;

fn failed(message: String) -> KernelError {
    KernelError::Failed { message }
}

fn plain(text: &str) -> String {
    text.to_owned()
}

/// 确认框之前：路径用不了或已在某个工作目录里时说为什么（有会话时记进对话，同 `/cd` 的失败，tui-manage.md §11.2）；
/// 能加时给出绝对路径，不加进去。有会话时按会话现在的目录判，没有会话时按草稿。
pub(crate) async fn check(
    kernel: &LocalKernel,
    request: AddDirectory,
) -> Result<DirectoryChecked, KernelError> {
    let directory = usable(kernel, &request).await?;
    let current = match &request.session {
        Some(session) => {
            kernel
                .open_session(session)
                .await?
                .view()
                .await?
                .additional_directories
        }
        None => request.drafts.clone(),
    };
    if let Some(covered) =
        working_dirs::covering(&request.workspace.root, &current, directory.as_str())
    {
        let input = request.path.trim();
        record(
            kernel,
            &request,
            working_dirs::covered_text(input, &covered, bold),
        )
        .await;
        return Err(failed(working_dirs::covered_text(input, &covered, plain)));
    }
    Ok(DirectoryChecked { directory })
}

/// 确认框里选了加：路径再检查一次（框开着时目录可能没了），有会话时交给 actor，没有会话时按草稿判定是否已在工作目录里。
pub(crate) async fn add(
    kernel: &LocalKernel,
    request: AddDirectory,
) -> Result<DirectoryAdded, KernelError> {
    let directory = usable(kernel, &request).await?;
    let input = request.path.trim();
    let Some(session) = &request.session else {
        if let Some(covered) =
            working_dirs::covering(&request.workspace.root, &request.drafts, directory.as_str())
        {
            return Err(failed(working_dirs::covered_text(input, &covered, plain)));
        }
        let remembered = if request.remember {
            remember(kernel, &request, &directory).await
        } else {
            Remembered::No
        };
        return Ok(DirectoryAdded {
            directory,
            remembered,
        });
    };
    let handle = kernel.open_session(session).await?;
    let envelope = CommandEnvelope {
        id: kernel.ports.ids.command(),
        command: Command::AddDirectory {
            directory: directory.clone(),
            args: request.path.clone(),
            remember: request.remember,
        },
    };
    match handle.send(envelope).await {
        Ack::Accepted {
            result: CommandResult::DirectoryAddition(Addition::Added { remembered }),
        } => Ok(DirectoryAdded {
            directory,
            remembered,
        }),
        Ack::Accepted {
            result: CommandResult::DirectoryAddition(Addition::Covered(covered)),
        } => Err(failed(working_dirs::covered_text(input, &covered, plain))),
        Ack::Rejected { reason, .. } => Err(failed(reason)),
        // 会话 actor 对 `AddDirectory` 只回 `DirectoryAddition`；命令 id 是新取的，不会是 `Duplicate`。
        other => Err(failed(format!("unexpected answer to /add-dir: {other:?}"))),
    }
}

/// 还没有会话时的「记住」：直接存进本机项目规则集（有会话时 actor 提交 `DirectoriesRemembered`）。
async fn remember(kernel: &LocalKernel, request: &AddDirectory, directory: &EnvPath) -> Remembered {
    match kernel
        .ports
        .store
        .remember_directories(&request.workspace, vec![directory.clone()])
        .await
    {
        Ok(()) => Remembered::Saved,
        Err(error) => Remembered::Failed {
            reason: error.to_string(),
        },
    }
}

/// 解析与 `stat`：相对路径按工作区根解析（会话的工作目录，session-dirs.md §5.1）。用不了时有会话就记进对话，文字交给前端。
async fn usable(kernel: &LocalKernel, request: &AddDirectory) -> Result<EnvPath, KernelError> {
    let input = request.path.trim();
    let root = &request.workspace.root;
    let machine = kernel.machine();
    let home = machine
        .info()
        .path_facts(&request.workspace)
        .await
        .ok()
        .and_then(|facts| facts.home);
    let checked = working_dirs::check(
        (machine.files().as_ref(), &request.workspace),
        PathStyle::infer(root.as_str()),
        (root.as_str(), home.as_ref().map(EnvPath::as_str)),
        input,
    )
    .await;
    match checked {
        Ok(directory) => Ok(EnvPath::new(directory)),
        Err(unusable) => {
            let recorded = working_dirs::unusable_text(input, &unusable, bold);
            record(kernel, request, recorded).await;
            Err(failed(working_dirs::unusable_text(input, &unusable, plain)))
        }
    }
}

/// 用不了：有会话时记进对话（缺省显示，加固定说明）；还没有会话时不记（同 `/cd`，tui-manage.md D25）。
async fn record(kernel: &LocalKernel, request: &AddDirectory, output: String) {
    let Some(session) = &request.session else {
        return;
    };
    if let Ok(handle) = kernel.open_session(session).await {
        let envelope = CommandEnvelope {
            id: kernel.ports.ids.command(),
            command: Command::RecordLocalCommand {
                record: LocalCommandRecord {
                    name: working_dirs::COMMAND.to_owned(),
                    args: request.path.clone(),
                    output: LocalCommandOutput::Stdout(output),
                    caveat: true,
                    meta: Vec::new(),
                },
            },
        };
        handle.send(envelope).await;
    }
}

#[cfg(test)]
#[path = "dirs_tests.rs"]
mod tests;
