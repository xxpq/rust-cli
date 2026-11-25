//! 自定义 agent：`~/.zcode/agents/*.md`（用户）与从 cwd 向上到 git 根每层的 `.zcode/agents/*.md`
//! （项目），经执行环境读取（docs/specs/rust-cli/agents.md §3）。

use std::collections::BTreeMap;

use icu_normalizer::ComposingNormalizerBorrowed;
use rust_cli_base::js;
use rust_cli_base::names::{AGENTS_DIR, DOT_DIR};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, ListDir, PathStyle, StatFile};
use rust_cli_kernel_api::env_git::project_levels;
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::permission::PermissionMode;

use crate::definitions::{AgentDefinition, AgentSource};
use crate::frontmatter::{self, Frontmatter, Value};

struct Reader<'a> {
    files: &'a dyn Files,
    workspace: &'a WorkspaceRef,
    /// 路径规则按执行环境的 platform（kernel-api `env_path`）。修复（R1-9）：以前按“路径含不含 `\`”
    /// 猜风格，`C:\a` 的上一层算成 `C:`。
    style: PathStyle,
}

impl Reader<'_> {
    async fn kind(&self, path: &str) -> Option<FileKind> {
        let request = StatFile {
            path: EnvPath::new(path),
        };
        match self.files.stat(self.workspace, request).await {
            Ok(found) => found.map(|stat| stat.kind),
            Err(error) => {
                tracing::debug!(%error, path, "agent directory not readable");
                None
            }
        }
    }

    /// 目录里的 `.md` 文件，按文件名排序（结果确定）。
    async fn markdown(&self, dir: &str) -> Vec<String> {
        let request = ListDir {
            path: EnvPath::new(dir),
        };
        let Ok(mut entries) = self.files.list_dir(self.workspace, request).await else {
            return Vec::new();
        };
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
            .into_iter()
            .filter(|entry| entry.kind == FileKind::File && entry.name.ends_with(".md"))
            .map(|entry| self.style.join(dir, &[&entry.name]))
            .collect()
    }

    async fn read(&self, path: &str) -> Option<String> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let content = self.files.read_file(self.workspace, request).await.ok()?;
        Some(String::from_utf8_lossy(&content.bytes).into_owned())
    }

    async fn load(&self, dir: &str, source: AgentSource) -> Vec<AgentDefinition> {
        let mut found = Vec::new();
        for file in self.markdown(dir).await {
            let parsed = self
                .read(&file)
                .await
                .and_then(|raw| definition(&frontmatter::read(&raw, &file), source));
            match parsed {
                Some(definition) => found.push(definition),
                None => tracing::debug!(file, "skipped an agent file without name or description"),
            }
        }
        found
    }

    /// cwd 向上到 git 根（含）或 home（不含）的 `.zcode/agents`，离 cwd 远的在前。遍历在 kernel-api（F58：以前
    /// 这里与 ext 各写一份）；`.git` 只认目录与文件（缺陷修复：以前任何种类都算，FIFO 等会让遍历提前停下）。
    async fn project_dirs(&self, facts: &EnvFacts) -> Vec<String> {
        let home = facts.home.as_ref().map(EnvPath::as_str);
        let levels = project_levels(
            self.files,
            self.workspace,
            self.style,
            facts.cwd.as_str(),
            home,
        );
        let mut dirs = Vec::new();
        for dir in levels.await {
            let agents = self.style.join(&dir, &[DOT_DIR, AGENTS_DIR]);
            if self.kind(&agents).await == Some(FileKind::Directory) {
                dirs.push(agents);
            }
        }
        dirs.reverse();
        dirs
    }
}

/// 先把 `manual` 换成 `default` 再校验（修复线 F58：以前不认 `manual`）。
fn permission_mode(value: &str) -> Option<PermissionMode> {
    PermissionMode::from_cli_choice(value)
}

/// agent 名字的长度上限（UTF-16 码元）。
const MAX_NAME_UNITS: usize = 256;
/// 留给插件命名空间的分隔符，自定义 agent 的名字里不能有（NFKC 之后）。
const NAMESPACE_SEPARATOR: char = ':';
/// `model` 写这个（不分大小写）时继承父会话的模型。
const INHERIT_MODEL: &str = "inherit";

/// 自定义 agent 的类型名（agents.md §3）：非空字符串，原样；以 `-` 开头、太长、NFKC 之后含 `:` 的不认（记日志）。
/// 修复：以前按标量的文字读、去首尾空白，数与布尔的名字和这几种名字都认了。
fn agent_type(map: &BTreeMap<String, Value>) -> Option<String> {
    let name = map.get("name")?.as_str().filter(|name| !name.is_empty())?;
    let refused = name.starts_with('-')
        || js::string::utf16_len(name) > MAX_NAME_UNITS
        || ComposingNormalizerBorrowed::new_nfkc()
            .normalize(name)
            .contains(NAMESPACE_SEPARATOR);
    if refused {
        tracing::warn!(name, "agent file has an invalid name");
        return None;
    }
    Some(name.to_owned())
}

/// 解析好的一个 agent 文件的定义；缺 `name` 或 `description` 时为 None（没有 frontmatter 的文件也是这样）。
fn definition(parsed: &Frontmatter, source: AgentSource) -> Option<AgentDefinition> {
    let map = &parsed.fields;
    let text = |key: &str| frontmatter::text(map, key);
    let agent_type = agent_type(map)?;
    // 描述要是非空字符串，原样（不去空白）；修复：以前数也认、首尾空白去掉。
    let when_to_use = map
        .get("description")?
        .as_str()
        .filter(|description| !description.is_empty())?
        .replace("\\n", "\n");
    let model = map
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(|model| {
            if model.eq_ignore_ascii_case(INHERIT_MODEL) {
                INHERIT_MODEL.to_owned()
            } else {
                model.to_owned()
            }
        });
    Some(AgentDefinition {
        agent_type,
        when_to_use,
        tools: frontmatter::tools(map.get("tools")),
        disallowed_tools: frontmatter::tools(map.get("disallowedTools")).unwrap_or_default(),
        prompt: parsed.body.trim().to_owned(),
        source,
        model,
        effort: text("effort"),
        max_turns: text("maxTurns")
            .and_then(|turns| turns.parse::<u32>().ok())
            .filter(|turns| *turns > 0),
        permission_mode: text("permissionMode").as_deref().and_then(permission_mode),
        background: text("background").as_deref() == Some("true"),
    })
}

/// 用户级、附加工作目录与项目级的自定义 agent（用户在前，项目在后；同名时后者覆盖）。附加工作目录的 `.zcode/agents`
/// 排在工作区的项目层级之前：同名时工作区的生效（session-dirs.md §3.3）。
pub(crate) async fn load(
    files: &dyn Files,
    workspace: &WorkspaceRef,
    facts: &EnvFacts,
    additional: &[EnvPath],
) -> Vec<Vec<AgentDefinition>> {
    let reader = Reader {
        files,
        workspace,
        style: PathStyle::of_platform(&facts.platform),
    };
    let user = match &facts.home {
        Some(home) => {
            reader
                .load(
                    &reader.style.join(home.as_str(), &[DOT_DIR, AGENTS_DIR]),
                    AgentSource::User,
                )
                .await
        }
        None => Vec::new(),
    };
    let mut layers = vec![user];
    for dir in additional {
        let agents = reader.style.join(dir.as_str(), &[DOT_DIR, AGENTS_DIR]);
        layers.push(reader.load(&agents, AgentSource::Project).await);
    }
    for dir in reader.project_dirs(facts).await {
        layers.push(reader.load(&dir, AgentSource::Project).await);
    }
    layers
}

#[cfg(test)]
#[path = "custom_tests.rs"]
mod tests;
