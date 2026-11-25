//! 会话的附加工作目录（docs/specs/rust-cli/session-dirs.md §3.2）：`Command::AddDirectory` 由会话 actor 判定。目录只有 actor
//! 一个 owner；路径的解析与 `stat` 在 runtime 做完（IO 不进 actor），这里只按自己的工作区根与附加目录做纯判定、追加、发事件，
//! 再把这次命令记进对话。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::session_api::{Ack, CommandResult, KernelEvent};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::user_command::{LocalCommandOutput, LocalCommandRecord, bold};
use rust_cli_kernel_api::working_dirs::{self, Addition, Remembered};

use crate::actor::Actor;

impl Actor {
    /// `/add-dir`：已在某个工作目录里时不改；否则追加、发 `DirectoryAdded`。`remember`（确认框里选了「记住」，permission-updates.md
    /// §6.1）时先提交 `DirectoriesRemembered`：存不上仍加进会话，文字写原因。两种结局都记进对话（缺省显示：固定说明、命令、
    /// 输出），回合在跑时留到收尾（`record_local_command`）。
    pub(crate) async fn add_directory(
        &mut self,
        command: CommandId,
        directory: EnvPath,
        args: String,
        remember: bool,
    ) -> Ack {
        let addition = match working_dirs::covering(
            &self.meta.workspace.root,
            &self.additional_directories,
            directory.as_str(),
        ) {
            Some(covered) => Addition::Covered(covered),
            None => {
                let remembered = if remember {
                    self.remember_directory(&directory).await
                } else {
                    Remembered::No
                };
                self.additional_directories.push(directory.clone());
                self.log.append(KernelEvent::DirectoryAdded {
                    directory: directory.clone(),
                });
                Addition::Added { remembered }
            }
        };
        let output = match &addition {
            Addition::Added { remembered } => {
                working_dirs::added_record(directory.as_str(), remembered)
            }
            Addition::Covered(covered) => working_dirs::covered_text(&args, covered, bold),
        };
        let record = LocalCommandRecord {
            name: working_dirs::COMMAND.to_owned(),
            args,
            output: LocalCommandOutput::Stdout(output),
            caveat: true,
            meta: Vec::new(),
        };
        self.record_local_command(command, record).await;
        Ack::Accepted {
            result: CommandResult::DirectoryAddition(addition),
        }
    }
}

impl Actor {
    /// 「记住」：本机项目规则集的 `additionalDirectories` 追加这个目录。提交不带命令 id：这次命令的效果记录是随后的本地命令记录。
    async fn remember_directory(&mut self, directory: &EnvPath) -> Remembered {
        let fact = Fact::DirectoriesRemembered {
            directories: vec![directory.clone()],
        };
        match self.commit(None, vec![fact]).await {
            Ok(()) => Remembered::Saved,
            Err(error) => Remembered::Failed {
                reason: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[path = "dirs_tests.rs"]
mod tests;
