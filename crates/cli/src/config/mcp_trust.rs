//! 工作区 MCP 的信任（docs/zcode-protocol-v5/18-mcp-trust.md §12）：项目级声明的 digest 与 Node 逐字节相同（ext 的
//! `mcp::declaration_digest`），信任记录与 Node CLI、App 共用（platform 的 `McpTrustStore`）。
//!
//! 组装时与执行的服务器同一次读文件，算出每个项目级声明的 digest（[`WorkspaceMcpTrust::of`]）；App 形态把它注入内核当准入端口，
//! 项目级服务器连接之前问它（§12.4）。`mcp.trust.set` 经 [`decide`] 在记录文件的锁里按磁盘上的声明重验再写（§12.3）。TUI 与 `-p`
//! 不用它（18 §10 审定第 4 条）。

use async_trait::async_trait;
use rust_cli_ext::mcp::{Declaration, McpScope, config_dir, declaration_digest};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::mcp::McpAdmission;
use rust_cli_paths::StoragePaths;
use rust_cli_platform::local::{ConfigFile, McpTable, McpTrustError, McpTrustKey, McpTrustStore};
use serde_json::Map;

use super::merge::{self, McpLayers};
use super::trust::storage_dir;
use super::{ConfigInputs, Files, file_layers, read_files};

/// 一个项目级声明：合并之后最终来源仍是项目级的服务器（用户级同名遮住的不算），与它那份声明的 digest。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpDeclaration {
    pub(crate) name: String,
    pub(crate) digest: String,
    /// 配置里启用（停用的不提示、不连）。
    pub(crate) enabled: bool,
}

/// 项目级声明与它们的 digest；有项目配置文件读不出时为 None（Node 的 `discover` 抛错：整个工作区没有可批准的声明）。
/// `windows`：Node 在 Windows 上拆开 `cmd /c`（产品代码给 `cfg!(windows)`，对拍测试两种都验）。
fn declarations(
    root: &str,
    files: &Files,
    mcp: &McpLayers,
    windows: bool,
) -> Option<Vec<McpDeclaration>> {
    let projects = &files.projects;
    if projects
        .iter()
        .any(|project| matches!(project.file, ConfigFile::Invalid { .. }))
    {
        return None;
    }
    let source_paths: Vec<String> = projects
        .iter()
        .map(|project| project.path.clone())
        .collect();
    let servers = mcp
        .servers
        .iter()
        .map(|server| (server, true))
        .chain(mcp.disabled.iter().map(|server| (server, false)));
    let found = servers
        .filter(|(server, _)| server.scope == McpScope::Project)
        .filter_map(|(server, enabled)| {
            // 合并时记下了最终定义所在的文件与表：digest 算的是那个文件里的原文（同 Node）。来源文件与相对 cwd 的基准目录按来源：
            // 项目配置是全部项目配置文件与那一层的目录，回落的 `.agents/mcp.json` 只有它自己与工作区根。
            let origin = mcp.origins.get(&server.name)?;
            let (object, sources, base) = match origin.table {
                McpTable::Config => {
                    let project = projects
                        .iter()
                        .find(|project| project.path == origin.path)?;
                    (
                        project.file.object()?,
                        source_paths.as_slice(),
                        config_dir(&project.path),
                    )
                }
                McpTable::Agents => {
                    let agents = files.fallback()?;
                    (
                        agents.file.object()?,
                        std::slice::from_ref(&agents.path),
                        root.to_owned(),
                    )
                }
            };
            let declared = origin.table.of(object)?.get(&server.name)?.as_object()?;
            let digest = declaration_digest(&Declaration {
                identity: root,
                root,
                name: &server.name,
                source_paths: sources,
                base: &base,
                server: declared,
                windows,
            });
            Some(McpDeclaration {
                name: server.name.clone(),
                digest,
                enabled,
            })
        })
        .collect();
    Some(found)
}

/// 一个工作区的项目级 MCP 声明与信任记录：准入端口（[`McpAdmission`]）与待信任的提示都从这里出。只拿快照与记录文件的位置，
/// 每次问都直接读记录（不缓存，没有文件监听，18 §12.3）。
#[derive(Debug, Clone)]
pub(crate) struct WorkspaceMcpTrust {
    /// 工作区身份：规范化的工作区根（Node 没有 Host 身份时的 `resolve(workingDirectory)`）。
    identity: String,
    declarations: Vec<McpDeclaration>,
    store: McpTrustStore,
}

/// 规范化的工作区根（Node `path.resolve` 对绝对路径的结果）：身份与 digest 的 `workingDirectory` 都用它。
fn normalized(root: &str) -> String {
    PathStyle::infer(root).normalize(root)
}

impl WorkspaceMcpTrust {
    /// 组装时：这一次读到的文件与合并结果（不做 IO）。
    pub(crate) fn of(root: &str, files: &Files, mcp: &McpLayers, paths: &StoragePaths) -> Self {
        let identity = normalized(root);
        Self {
            declarations: declarations(&identity, files, mcp, cfg!(windows)).unwrap_or_default(),
            store: McpTrustStore::new(paths.mcp_trust_store(storage_dir(files.user.object()))),
            identity,
        }
    }

    fn key<'a>(&'a self, declaration: &'a McpDeclaration) -> McpTrustKey<'a> {
        McpTrustKey {
            identity: &self.identity,
            server: &declaration.name,
            digest: &declaration.digest,
        }
    }

    /// 启用、在现在的记录里没有精确 digest 的声明（`mcpAdmission` 的输入，18 §12.5）：名字与 digest，按名字。记录读不出时全部算。
    pub(crate) async fn pending(&self) -> Vec<(String, String)> {
        let load = self.store.load().await;
        self.declarations
            .iter()
            .filter(|declaration| declaration.enabled && !load.trusts(&self.key(declaration)))
            .map(|declaration| (declaration.name.clone(), declaration.digest.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn declarations(&self) -> &[McpDeclaration] {
        &self.declarations
    }
}

#[async_trait]
impl McpAdmission for WorkspaceMcpTrust {
    /// 快照里这个名字的那份声明有信任记录才准入；不是项目级声明（不在快照里）或记录读不出时不准入。
    async fn admits(&self, server: &str) -> bool {
        let Some(declaration) = self.declarations.iter().find(|known| known.name == server) else {
            return false;
        };
        self.store.load().await.trusts(&self.key(declaration))
    }
}

/// `mcp.trust.set` 要的决定。
pub(crate) struct McpDecision<'a> {
    pub(crate) server: &'a str,
    pub(crate) digest: &'a str,
    pub(crate) trusted: bool,
}

/// 磁盘上现在的项目级声明（锁里重读）。
async fn current(inputs: &ConfigInputs<'_>, windows: bool) -> Option<Vec<McpDeclaration>> {
    let files = read_files(inputs).await;
    let empty = Map::new();
    let mut warnings = Vec::new();
    let layers = file_layers(&files, &empty, &mut warnings);
    let mcp = merge::mcp_layers(&layers, files.fallbacks(), &mut warnings);
    declarations(
        &normalized(inputs.workspace.root.as_str()),
        &files,
        &mcp,
        windows,
    )
}

/// 信任或撤销一份声明（Node `decide`，18 §12.3）：记录文件的锁里重读磁盘上的声明，这个名字的 digest 与 `decision` 的相同才写。
/// 返回有没有改文件（结果已是现状时不写）。
pub(crate) async fn decide(
    inputs: &ConfigInputs<'_>,
    decision: &McpDecision<'_>,
    granted_at: &str,
) -> Result<bool, McpTrustError> {
    let files = read_files(inputs).await;
    let store = McpTrustStore::new(
        inputs
            .paths
            .mcp_trust_store(storage_dir(files.user.object())),
    );
    let identity = normalized(inputs.workspace.root.as_str());
    let key = McpTrustKey {
        identity: &identity,
        server: decision.server,
        digest: decision.digest,
    };
    store
        .decide(&key, decision.trusted, granted_at, async || {
            let declared = current(inputs, cfg!(windows))
                .await
                .ok_or(McpTrustError::ConfigUnreadable)?;
            let same = declared
                .iter()
                .any(|known| known.name == decision.server && known.digest == decision.digest);
            same.then_some(()).ok_or(McpTrustError::DeclarationChanged)
        })
        .await
}

#[cfg(test)]
#[path = "mcp_trust_tests.rs"]
mod tests;
