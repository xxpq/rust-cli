//! bash 模式（用户在输入框里输入 `!命令`，docs/specs/rust-cli/tui-input.md §2.2）：经 Bash 工具执行，结果拼成
//! `<bash-stdout>` / `<bash-stderr>` 两段。
use rust_cli_kernel_api::tool::{ToolError, ToolProgress, ToolUseContext};
use rust_cli_kernel_api::user_input::{BashRun, escape_xml};
use tokio::sync::mpsc;

use crate::bash::{BashTool, Ran};

/// 大输出的预览不转义（它的标签要留给模型与界面解析）。
const PERSISTED: &str = "<persisted-output>";

pub(crate) async fn run(
    bash: &BashTool,
    command: &str,
    context: ToolUseContext,
    progress: mpsc::Sender<ToolProgress>,
) -> BashRun {
    match bash.run_typed(command, context, progress).await {
        // 成功：stdout 是 Bash 结果的文字（按 `stderr: ''` 映射），stderr 为空
        // （合流的输出都在 stdout 里）。
        // bash 模式不转后台（`run_typed` 不许超时转后台）；被用户的转后台键转走时同样只给说明文字。
        // 只给界面的输出头部不用：bash 模式的结果是给模型的文字（`<persisted-output>` 原样留给模型与界面解析）。
        Ok(Ran::Text(text, _) | Ran::Launched(text, _)) => BashRun::Completed {
            stdout: if text.starts_with(PERSISTED) {
                text
            } else {
                escape_xml(&text)
            },
            stderr: String::new(),
        },
        // 退出码表示失败：合流的输出放进 stderr，不带 `Exit code N`。
        Ok(Ran::Failed { output, .. }) => BashRun::Completed {
            stdout: String::new(),
            stderr: escape_xml(&output),
        },
        Err(ToolError::Cancelled) => BashRun::Interrupted,
        Err(error) => BashRun::Failed {
            message: error.to_string(),
        },
    }
}
