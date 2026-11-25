//! 执行更新（docs/specs/rust-cli/update.md §8）：TUI 退出、终端恢复之后，按交接变量与平台选做法
//! （[`handoff::route`]）。Unix 经 proc 起包管理器，stdio 继承，打印 update.md §8 的文字；Windows 写交接文件由
//! 启动器在本进程退出后执行（§8.1，工作线 F66），直接运行时只打印命令。失败是 `flags::Fatal`（入口原样写 stderr，
//! 退出码 1）。

use std::io::Write;

use anyhow::Result;

use super::handoff::{self, Route};
use super::install::Install;
use crate::commands::flags::Fatal;

/// 一次更新：在跑的版本与要装的版本（update.md §8）。
///
/// 2026-11-12 起（用户要求“自动更新之后把更新前后的版本号打印一下，不然不知道升级到啥版本了”）总有确定的目标版本：
/// 选择屏的 Update now 是选择屏上的最新版本，`zcode update` 是先查到的最新版本。命令带 `@<目标>`，装的就是打印的版本；
/// 以前 Unix 的命令不带版本，包管理器装它看到的 latest，打印不出装了哪个。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target<'a> {
    pub(crate) from: &'a str,
    pub(crate) to: &'a str,
}

/// 执行并打印结果；成功时返回退出码 0。`env` 是入口的进程环境快照（子进程照它继承，Unix 上按它的 `PATH` 找程序）。
pub(crate) async fn run(
    install: &Install,
    target: Target<'_>,
    env: &rust_cli_base::secret::EnvSnapshot,
    out: &mut dyn Write,
) -> Result<u8> {
    execute(
        handoff::route(env, cfg!(windows)),
        install,
        target,
        env,
        out,
    )
    .await
}

/// [`run`] 按选好的做法执行（测试在本机按 Windows 的做法验证）。
pub(crate) async fn execute(
    route: Route,
    install: &Install,
    target: Target<'_>,
    env: &rust_cli_base::secret::EnvSnapshot,
    out: &mut dyn Write,
) -> Result<u8> {
    match route {
        // 交接：不打印（启动器打印与 Unix 相同的文字），以 0 退出。
        Route::Handoff(path) => {
            handoff::write(&path, target.to)
                .await
                .map_err(|error| Fatal(format!("Error: {error}")))?;
            Ok(0)
        }
        Route::Manual => Err(Fatal(handoff::manual(&install.command_line(Some(target.to)))).into()),
        Route::Run => run_here(install, target, env, out).await,
    }
}

/// Unix：本进程起包管理器。F66 之前 Windows 也走这里（只在 `PATH` 的绝对路径项里找命令、工作目录是私有临时目录），
/// 但 npm 换不掉正在运行的可执行文件（Windows 平台包里的 `zcode.exe`）；这部分搬到了 Windows 的启动器（update.md §8.1），这里只剩 Unix 的做法。
async fn run_here(
    install: &Install,
    Target { from, to }: Target<'_>,
    env: &rust_cli_base::secret::EnvSnapshot,
    out: &mut dyn Write,
) -> Result<u8> {
    let line = install.command_line(Some(to));
    // 终端已经关掉时写不进去也照常执行（与恢复提示相同，写失败不 panic）。
    let _ = writeln!(out, "\nUpdating ZCode {from} → {to} via `{line}`...");
    let _ = out.flush();
    let mut command = rust_cli_proc::command(install.program());
    command
        .args(install.args(Some(to)))
        .env_clear()
        .envs(env.vars().iter().cloned());
    let status = command
        .status()
        .await
        .map_err(|error| Fatal(format!("Error: {error}")))?;
    if !status.success() {
        return Err(Fatal(format!("Error: `{line}` failed with status {status}")).into());
    }
    let _ = writeln!(
        out,
        "\n🎉 Updated ZCode from {from} to {to}. Please restart ZCode."
    );
    let _ = out.flush();
    Ok(0)
}

#[cfg(test)]
#[path = "action_tests.rs"]
mod tests;
