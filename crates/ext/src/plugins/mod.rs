//! 插件（docs/specs/rust-cli/ext.md §4）：目录与 manifest，提供 skill、命令、MCP 服务器、agent 文件与 hooks。
//! 候选目录、启停、配置值与数据根由入口层给出（[`PluginSources`]，§4.0、§4.12）；文件经主 env 的 `Files` 读。一次加载
//! 的结果是不可变的 [`PluginSet`]，内核只收它。来源与启用判定见 [`select`]。
//!
//! manifest 按 Node `findManifest` 依次找本工程的 `.zcode-plugin` 与另外两种插件格式的目录下的 `plugin.json`
//! （同时兼容几种插件格式）；`--plugin-dir` 没有 manifest 时插件名取目录名。组件的路径规则见 §4.5。

mod components;
mod entries;
mod select;
mod set;

use std::sync::Arc;

use rust_cli_base::plugin::PluginContext;
use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, ListDir, PathStyle, StatFile};
use rust_cli_kernel_api::machine::Files;
use serde_json::Value;

use crate::mcp::McpServer;
use crate::skills::PluginSkillRoot;

pub use self::components::{CommandMetadata, PluginCommandRoot};
pub use self::entries::PluginEntry;
pub use self::select::{
    NodeDependency, OfficialTable, PluginCandidate, PluginOrigin, PluginSources, SkipReason,
    Skipped, Unhosted, default_enabled, load,
};
pub use self::set::{PluginError, PluginErrorKind, PluginSet};

/// manifest 的位置，按 Node `findManifest` 的顺序。
const MANIFESTS: [&str; 3] = [
    ".zcode-plugin/plugin.json",
    ".claude-plugin/plugin.json",
    ".codex-plugin/plugin.json",
];

/// 读 manifest 的结果。
enum Manifest {
    Missing,
    Invalid(String),
    Found(Value),
}

/// 插件里的一个 agent 文件（agents.md §3 按插件规则解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAgentFile {
    pub path: String,
    /// 相对 agents 目录的子目录（namespace）；manifest 直接列出的文件为空。
    pub namespace: Vec<String>,
}

/// 一个已加载的插件。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plugin {
    /// id、名字、目录、数据目录与配置值（hooks、agents、skill 正文的变量展开都用它，ext.md §4.10）。
    pub context: Arc<PluginContext>,
    /// 插件 id 的后半（候选的 marketplace；`--plugin-dir` 是 `inline`）。
    pub marketplace: String,
    pub origin: PluginOrigin,
    /// manifest 原样（未知键保留，§4.3 第 2 步）。
    pub manifest: Value,
    /// skill 根目录：每个子目录一个 `SKILL.md`，或者自己就有 `SKILL.md`（§4.5）。
    pub skill_dirs: Vec<String>,
    /// 命令的根（§4.5.1）。
    pub command_roots: Vec<PluginCommandRoot>,
    pub mcp_servers: Vec<McpServer>,
    /// agent 文件（runtime 交给 agents）。
    pub agent_files: Vec<PluginAgentFile>,
    /// `hooks/hooks.json` 与 manifest `hooks` 的内容，都是文件的 `{"hooks": …}` 形状（runtime 交给 hooks 引擎）。
    pub hooks: Vec<Value>,
}

impl Plugin {
    pub fn id(&self) -> &str {
        &self.context.id
    }

    pub fn name(&self) -> &str {
        &self.context.name
    }

    pub fn dir(&self) -> &str {
        &self.context.root
    }

    /// manifest 的字符串版本，去掉控制字符后为空时没有（headless 的 `system/init`，ext.md §4.14）。
    pub fn version(&self) -> Option<String> {
        let version = self.manifest.get("version")?.as_str()?;
        let clean: String = version.chars().filter(|c| !c.is_control()).collect();
        (!clean.trim().is_empty()).then_some(clean)
    }

    pub(crate) fn skill_roots(&self) -> impl Iterator<Item = PluginSkillRoot> + '_ {
        self.skill_dirs.iter().map(|dir| PluginSkillRoot {
            plugin: self.name().to_owned(),
            dir: dir.clone(),
        })
    }
}

/// 插件目录下的路径（`path.join`）：风格按插件目录的写法推断，规则是 kernel-api `env_path`
/// （R1-9：以前按“含不含 `\`”猜风格，自己拼接）。
fn join(base: &str, relative: &str) -> String {
    PathStyle::infer(base).join(base, &[relative])
}

fn base_name(path: &str) -> String {
    PathStyle::infer(path).file_name(path)
}

/// manifest 里的组件路径：解析到插件目录下；越出插件目录时为 None（Node `resolveInside`，ext.md §4.5）。
fn inside(root: &str, relative: &str) -> Option<String> {
    let style = PathStyle::infer(root);
    let base = style.normalize(root);
    let resolved = style.resolve(&base, relative, None);
    let under = resolved
        .strip_prefix(base.as_str())
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(style.separator()));
    under.then_some(resolved)
}

/// manifest 里的路径字段：字符串或字符串数组。
fn paths(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(path)) => vec![path.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

struct Reader<'a> {
    files: &'a dyn Files,
    workspace: &'a WorkspaceRef,
}

impl Reader<'_> {
    async fn kind(&self, path: &str) -> Option<FileKind> {
        let request = StatFile {
            path: EnvPath::new(path),
        };
        self.files
            .stat(self.workspace, request)
            .await
            .ok()
            .flatten()
            .map(|stat| stat.kind)
    }

    async fn parse(&self, path: &str) -> Result<Value, String> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let content = self
            .files
            .read_file(self.workspace, request)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&content.bytes).map_err(|error| error.to_string())
    }

    async fn json(&self, path: &str) -> Option<Value> {
        match self.parse(path).await {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(path, %error, "plugin file is not valid JSON");
                None
            }
        }
    }

    /// 目录里的 `.md` 文件，递归进子目录，子目录名记作命名空间；按名字
    /// 排序，结果确定。
    async fn markdown(&self, root: &str) -> Vec<PluginAgentFile> {
        let mut found = Vec::new();
        let mut pending = vec![(root.to_owned(), Vec::<String>::new())];
        while let Some((dir, namespace)) = pending.pop() {
            let request = ListDir {
                path: EnvPath::new(dir.as_str()),
            };
            let Ok(mut entries) = self.files.list_dir(self.workspace, request).await else {
                continue;
            };
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            let mut subdirs = Vec::new();
            for entry in entries {
                let path = join(&dir, &entry.name);
                match entry.kind {
                    FileKind::Directory => {
                        let mut inner = namespace.clone();
                        inner.push(entry.name);
                        subdirs.push((path, inner));
                    }
                    FileKind::File if entry.name.to_lowercase().ends_with(".md") => {
                        found.push(PluginAgentFile {
                            path,
                            namespace: namespace.clone(),
                        });
                    }
                    _ => {}
                }
            }
            // 先处理本层文件，再按名字顺序进子目录。
            pending.extend(subdirs.into_iter().rev());
        }
        found
    }

    async fn manifest(&self, dir: &str) -> Manifest {
        for candidate in MANIFESTS {
            let path = join(dir, candidate);
            if self.kind(&path).await == Some(FileKind::File) {
                return match self.parse(&path).await {
                    Ok(value) => Manifest::Found(value),
                    Err(error) => Manifest::Invalid(format!("{path}: {error}")),
                };
            }
        }
        Manifest::Missing
    }
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
