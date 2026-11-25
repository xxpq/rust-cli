//! 主 env 上路径的依据与文件类数据的位置（docs/specs/rust-cli/deployment.md §8、§19.1、§21）。
//!
//! [`PrimaryPaths`]：主 env 报告的 home 与路径风格。路径一律指执行侧，命令行的路径参数与记忆目录都按它解析，controller
//! 不用自己的 cwd 与 home。
//!
//! [`EnvLayout`]：自动记忆目录与插件数据目录（docs/specs/rust-cli/ext.md §4.9）。主 env 在本进程里时位置照旧用 controller 的存储
//! 路径（与 Node 共用、跟随 `storage.dir`，行为不变）；主 env 在别处时按它报告的 home 与路径风格拼成 Node 的缺省布局（controller 的
//! 存储路径在那台机器上不存在）。都经主 env 的 `Files::ensure_dir` 建（插件数据目录由 ext 在接纳插件时建）。

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::{EnsureDir, PathStyle};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_paths::{StoragePath, StoragePaths};

use crate::config::plugins::ExecutionSide;

/// 主 env 报告的 home 与路径风格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrimaryPaths {
    pub(crate) home: Option<String>,
    pub(crate) style: PathStyle,
}

impl PrimaryPaths {
    /// 问主 env 要 home 与路径风格；问不到时没有 home（`~` 当普通相对路径，远端的记忆关闭），风格按工作区根推断。
    pub(crate) async fn of(machine: &MachineEnv, workspace: &WorkspaceRef) -> Self {
        match machine.info().path_facts(workspace).await {
            Ok(facts) => Self {
                home: facts.home.map(|home| home.as_str().to_owned()),
                style: facts.style,
            },
            Err(error) => {
                tracing::warn!(%error, "the primary env reported no path facts");
                Self {
                    home: None,
                    style: PathStyle::infer(workspace.root.as_str()),
                }
            }
        }
    }

    /// 用户写的路径在主 env 上的绝对路径：`~` 按主 env 的 home，相对路径对着会话的工作区。
    pub(crate) fn resolve(&self, workspace: &WorkspaceRef, input: &str) -> EnvPath {
        EnvPath::new(
            self.style
                .resolve(workspace.root.as_str(), input, self.home.as_deref()),
        )
    }
}

/// 主 env 上文件类数据的布局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvLayout {
    /// 主 env 在本进程里：位置是 controller 的存储路径。
    Controller,
    /// 主 env 在别处：按那台机器的 home（没报告时记忆关闭）与路径风格。
    Remote(PrimaryPaths),
}

impl EnvLayout {
    /// 插件的执行侧（ext.md §4.0、§4.9）：数据根，与插件存储（控制侧的内置与已安装插件）在不在同一个文件系统。
    /// `plugin_root` 是插件存储根。
    pub(crate) fn plugin_side(&self, plugin_root: &StoragePath) -> ExecutionSide {
        match self {
            Self::Controller => ExecutionSide {
                data_root: Some(EnvPath::new(
                    plugin_root.plugin_data().as_path().display().to_string(),
                )),
                shares_storage: true,
            },
            Self::Remote(primary) => ExecutionSide {
                data_root: primary.home.as_deref().map(|home| {
                    EnvPath::new(
                        primary
                            .style
                            .join(home, &rust_cli_paths::home_plugin_data_dir()),
                    )
                }),
                shares_storage: false,
            },
        }
    }

    /// 自动记忆目录：建好后返回给提示词的展示文本（末尾带分隔符）；建不了时为 None（提示词会说目录已存在，所以建
    /// 不了就不打开记忆段）。`project` 是项目的规范路径。
    pub(crate) async fn memory_dir(
        &self,
        machine: &MachineEnv,
        paths: &StoragePaths,
        workspace: &WorkspaceRef,
        project: &str,
    ) -> Option<String> {
        let (dir, separator) = match self {
            Self::Controller => (
                paths.memory_dir(project).as_path().display().to_string(),
                std::path::MAIN_SEPARATOR,
            ),
            Self::Remote(primary) => {
                let segments = rust_cli_paths::home_memory_dir(project);
                let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
                let dir = primary.style.join(primary.home.as_deref()?, &parts);
                (dir, primary.style.separator())
            }
        };
        let request = EnsureDir {
            path: EnvPath::new(dir.as_str()),
        };
        machine.files().ensure_dir(workspace, request).await.ok()?;
        Some(format!("{dir}{separator}"))
    }
}
