//! skill 目录的发现与 SKILL.md 的解析（docs/specs/rust-cli/ext.md §2.1、§2.2）；旧的 `commands/` 目录在 [`super::commands`]（§2.10）。

use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::future::join_all;
use rust_cli_base::names::{DOT_AGENTS_DIR, DOT_DIR, managed_dir};
use rust_cli_base::text;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, FileStat, ListDir, PathStyle, StatFile, StatMany};
use rust_cli_kernel_api::env_git::project_levels;
use rust_cli_kernel_api::machine::Files;

use super::{Skill, SkillCatalog, SkillShell, SkillSource};
use crate::plugins::{Plugin, PluginCommandRoot};
use rust_cli_base::frontmatter::{self, Value};
use rust_cli_kernel_api::tool_names;
use rust_cli_kernel_api::user_command::UserCommandKind;

/// skill 的文件名。
pub(super) const SKILL_FILE: &str = "SKILL.md";
/// SKILL.md 的大小上限（1e6 字节），超过时跳过。
pub(super) const MAX_SKILL_BYTES: u64 = 1_000_000;

/// 每个根目录下的 skill 目录（`<根>/.zcode/skills`、`<根>/.agents/skills`）。
pub(super) const SKILLS_DIR: &str = "skills";
/// 没有 description 时取正文第一行，最多这么多个字符（超过时截断加 `...`）。
const MAX_DESCRIPTION_CHARS: usize = 100;
/// 旧 `commands/` 目录的命令正文为空时的描述（§2.10，对照实现解析命令时传的标签）。
const COMMAND_DESCRIPTION: &str = "Custom command";

/// 一个根目录与它的来源。
struct Root {
    dir: String,
    source: SkillSource,
}

fn base_roots(style: PathStyle, base: &str, source: &SkillSource) -> [Root; 2] {
    // `.zcode` 与 `.agents` 合并，同级 `.zcode` 在前（Node `skillRootsForBase`）。
    [DOT_DIR, DOT_AGENTS_DIR].map(|dot| Root {
        dir: style.join(base, &[dot, SKILLS_DIR]),
        source: source.clone(),
    })
}

pub(super) struct Reader<'a> {
    catalog: &'a SkillCatalog,
    pub(super) files: &'a dyn Files,
    pub(super) workspace: &'a WorkspaceRef,
    /// 路径规则按执行环境的 platform（kernel-api `env_path`）。修复（R1-9）：以前按“路径含不含 `\`”
    /// 猜风格，`C:\a` 的上一层算成 `C:`。
    pub(super) style: PathStyle,
}

impl<'a> Reader<'a> {
    pub(super) fn new(
        catalog: &'a SkillCatalog,
        workspace: &'a WorkspaceRef,
        style: PathStyle,
    ) -> Self {
        Self {
            catalog,
            files: catalog.files.as_ref(),
            workspace,
            style,
        }
    }
}

impl Reader<'_> {
    /// cwd 向上到 git 根（含）或 home（不含）的每一层，离 cwd 近的在前。遍历在 kernel-api（F58：以前这里与
    /// agents 各写一份）；`.git` 只认目录与文件
    /// （缺陷修复：以前任何种类都算，FIFO 等会让遍历提前停下）。
    pub(super) async fn project_levels(&self, facts: &EnvFacts) -> Vec<String> {
        let home = facts.home.as_ref().map(EnvPath::as_str);
        project_levels(
            self.files,
            self.workspace,
            self.style,
            facts.cwd.as_str(),
            home,
        )
        .await
    }

    /// 一个根目录里的 skill，按名字排序（`localeCompare`）。
    async fn load(&self, root: &Root) -> Vec<Arc<Skill>> {
        let candidates = self
            .subdirectories(&root.dir)
            .await
            .into_iter()
            .map(|name| {
                let dir = self.style.join(&root.dir, &[&name]);
                let source = root.source.clone();
                Candidate::new(
                    self.style.join(&dir, &[SKILL_FILE]),
                    MAX_SKILL_BYTES,
                    move |raw| parse(raw, name, &dir, source),
                )
            })
            .collect();
        self.parse_all(candidates).await
    }

    /// 一个插件 skill 根里的 skill（ext.md §4.5）：根自己有 `SKILL.md` 时就是一个 skill（名字取根的目录名），否则每个子
    /// 目录一个。`allowed-tools` 里的插件目录与数据目录变量换掉（§4.10）。根的 `SKILL.md` 与子目录同时查（§2.1 末条）。
    async fn plugin_skills(&self, plugin: &Plugin, dir: &str) -> Vec<Arc<Skill>> {
        let direct = self.style.join(dir, &[SKILL_FILE]);
        let request = StatFile {
            path: EnvPath::new(&direct),
        };
        let (stat, entries) = futures_util::join!(
            self.files.stat(self.workspace, request),
            self.subdirectories(dir)
        );
        let is_skill = matches!(stat, Ok(Some(stat)) if stat.kind == FileKind::File);
        let named: Vec<(String, String)> = if is_skill {
            let name = format!("{}:{}", plugin.name(), self.style.file_name(dir));
            vec![(dir.to_owned(), name)]
        } else {
            entries
                .into_iter()
                .map(|entry| {
                    let name = format!("{}:{entry}", plugin.name());
                    (self.style.join(dir, &[&entry]), name)
                })
                .collect()
        };
        let candidates = named
            .into_iter()
            .map(|(dir, name)| {
                let source = SkillSource::Plugin {
                    plugin: plugin.name().to_owned(),
                };
                Candidate::new(
                    self.style.join(&dir, &[SKILL_FILE]),
                    MAX_SKILL_BYTES,
                    move |raw| {
                        let mut skill = parse(raw, name, &dir, source);
                        expand_allowed_tools(&mut skill, plugin);
                        skill
                    },
                )
            })
            .collect();
        self.parse_all(candidates).await
    }

    /// 目录下的子目录名，按名字排序（`localeCompare`）。
    async fn subdirectories(&self, dir: &str) -> Vec<String> {
        let request = ListDir {
            path: EnvPath::new(dir),
        };
        let Ok(mut entries) = self.files.list_dir(self.workspace, request).await else {
            return Vec::new();
        };
        entries.retain(|entry| entry.kind == FileKind::Directory);
        entries.sort_by_key(|entry| (entry.name.to_lowercase(), entry.name.clone()));
        entries.into_iter().map(|entry| entry.name).collect()
    }

    /// 一批文件的解析结果，顺序同 `candidates`（§2.1 末条）：一次 `stat_many`（远端机器合成一个请求），缓存没命中的
    /// 并发读（最多 [`READ_CONCURRENCY`] 个）。修改：以前逐个 `stat`、逐个读，每个文件至少一次往返，远端机器上
    /// 46 个 skill 每一步 46 次以上往返（`load_latency_tests`）。
    pub(super) async fn parse_all(&self, candidates: Vec<Candidate<'_>>) -> Vec<Arc<Skill>> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let request = StatMany {
            paths: candidates
                .iter()
                .map(|candidate| EnvPath::new(&candidate.file))
                .collect(),
        };
        let stats = match self.files.stat_many(self.workspace, request).await {
            Ok(stats) => stats,
            Err(error) => {
                tracing::debug!(%error, "skill files are not available");
                return Vec::new();
            }
        };
        let mut parsed = Vec::with_capacity(candidates.len());
        for (candidate, stat) in candidates.into_iter().zip(stats) {
            parsed.push(self.parsed(candidate, stat.ok().flatten()));
        }
        futures_util::stream::iter(parsed)
            .buffered(READ_CONCURRENCY)
            .filter_map(std::future::ready)
            .collect()
            .await
    }

    /// 一个普通文件的解析结果：不在、不是普通文件、超过上限时没有；按路径与版本缓存（§2.1 末条）。
    async fn parsed(&self, candidate: Candidate<'_>, stat: Option<FileStat>) -> Option<Arc<Skill>> {
        let Candidate { file, limit, parse } = candidate;
        let stat = stat?;
        if stat.kind != FileKind::File {
            return None;
        }
        if stat.size > limit {
            tracing::warn!(
                file,
                size = stat.size,
                "skipping a skill over the size limit"
            );
            return None;
        }
        if let Some(skill) = self.catalog.cached(&file, &stat.version) {
            return Some(skill);
        }
        let request = ReadFile {
            path: EnvPath::new(&file),
        };
        let content = self.files.read_file(self.workspace, request).await.ok()?;
        let raw = String::from_utf8_lossy(&content.bytes);
        let skill = Arc::new(parse(&raw));
        self.catalog.remember(file, content.version, skill.clone());
        Some(skill)
    }
}

/// 发现时同时读的文件数上限：首次发现时缓存全空，插件多时一次要读几百个文件。
const READ_CONCURRENCY: usize = 32;

/// 一个要解析的文件：路径、大小上限与解析函数（[`Reader::parse_all`]）。
pub(super) struct Candidate<'a> {
    file: String,
    limit: u64,
    parse: Box<dyn FnOnce(&str) -> Skill + Send + 'a>,
}

impl<'a> Candidate<'a> {
    pub(super) fn new(
        file: String,
        limit: u64,
        parse: impl FnOnce(&str) -> Skill + Send + 'a,
    ) -> Self {
        Self {
            file,
            limit,
            parse: Box::new(parse),
        }
    }
}

/// 插件 skill 与命令的 `allowed-tools` 里的插件变量（ext.md §4.10）。
pub(super) fn expand_allowed_tools(skill: &mut Skill, plugin: &Plugin) {
    for tool in &mut skill.allowed_tools {
        *tool = plugin.context.expand_paths(tool);
    }
}

/// 一次发现的目录（ext.md §2.1）：项目层级、skill 根，与要订阅变化的目录（§2.1.1）。
pub(crate) struct Layout {
    levels: Vec<String>,
    roots: Vec<Root>,
    /// skill 与命令的根目录（managed、用户、项目每层、附加目录的 `.zcode`、`.agents` 下的 `skills`、`commands`），加上插件的
    /// skill 根与命令根所在的目录：发现读的全部目录。
    pub(crate) watched: Vec<EnvPath>,
}

/// 算出这一次发现要读的目录：项目层级只算一次，skill 与命令共用。
pub(crate) async fn layout(
    catalog: &SkillCatalog,
    workspace: &WorkspaceRef,
    (facts, additional): (&EnvFacts, &[EnvPath]),
) -> Layout {
    let style = PathStyle::of_platform(&facts.platform);
    let reader = Reader {
        catalog,
        files: catalog.files.as_ref(),
        workspace,
        style,
    };
    let levels = reader.project_levels(facts).await;
    let managed = managed_dir(&facts.platform);
    let mut roots = vec![Root {
        dir: style.join(managed, &[DOT_DIR, SKILLS_DIR]),
        source: SkillSource::Managed,
    }];
    let mut bases = vec![managed.to_owned()];
    if let Some(home) = &facts.home {
        roots.extend(base_roots(style, home.as_str(), &SkillSource::User));
        bases.push(home.as_str().to_owned());
    }
    for level in &levels {
        roots.extend(base_roots(style, level, &SkillSource::Project));
        bases.push(level.clone());
    }
    for dir in additional {
        roots.extend(base_roots(style, dir.as_str(), &SkillSource::Project));
        bases.push(dir.as_str().to_owned());
    }
    let mut watched: Vec<EnvPath> = bases
        .iter()
        .flat_map(|base| {
            [DOT_DIR, DOT_AGENTS_DIR].into_iter().flat_map(move |dot| {
                [SKILLS_DIR, super::commands::COMMANDS_DIR]
                    .map(|kind| EnvPath::new(style.join(base, &[dot, kind])))
            })
        })
        .collect();
    for plugin in catalog.plugins().plugins() {
        watched.extend(plugin.skill_dirs.iter().map(EnvPath::new));
        watched.extend(plugin.command_roots.iter().map(|root| match root {
            PluginCommandRoot::Dir(dir) => EnvPath::new(dir),
            PluginCommandRoot::File { path, .. } => EnvPath::new(style.dirname(path)),
        }));
    }
    Layout {
        levels,
        roots,
        watched,
    }
}

/// 全部来源的 skill，按 ext.md §2.1 的顺序拼接（未去重）。会话的附加工作目录（session-dirs.md §3.3）的 `.zcode/skills`、
/// `.agents/skills` 排在工作区的项目层级之后、插件之前，来源同项目级。插件的命令排在插件 skill 之前（§4.5.1）。
/// 各个根目录同时读，结果按上面的顺序拼（§2.1 末条）。
pub(crate) async fn discover(
    catalog: &SkillCatalog,
    workspace: &WorkspaceRef,
    (facts, additional): (&EnvFacts, &[EnvPath]),
    layout: &Layout,
) -> Vec<Arc<Skill>> {
    let reader = Reader {
        catalog,
        files: catalog.files.as_ref(),
        workspace,
        style: PathStyle::of_platform(&facts.platform),
    };
    let plugins = catalog.plugins().plugins();
    // 先收成 Vec 再 await：迭代器的闭包跨过 await 时 rustc 推不出 Send（rust-lang/rust#102211）。
    let skills: Vec<_> = layout.roots.iter().map(|root| reader.load(root)).collect();
    let plugin_commands: Vec<_> = plugins
        .iter()
        .map(|plugin| super::commands::plugin(&reader, plugin))
        .collect();
    let mut plugin_skills = Vec::new();
    for plugin in plugins {
        for dir in &plugin.skill_dirs {
            plugin_skills.push(reader.plugin_skills(plugin, dir));
        }
    }
    let (skills, commands, plugin_commands, plugin_skills) = futures_util::join!(
        join_all(skills),
        // 旧的 `commands/` 目录排在全部文件 skill 之后、插件之前（§2.10）
        super::commands::discover(&reader, facts, &layout.levels, additional),
        join_all(plugin_commands),
        join_all(plugin_skills),
    );
    let mut found: Vec<Arc<Skill>> = skills.into_iter().flatten().collect();
    found.extend(commands);
    found.extend(plugin_commands.into_iter().flatten());
    found.extend(plugin_skills.into_iter().flatten());
    found
}

/// 标量字段的文字（数与布尔按 JS 的 `String(v)`）；`null`、列表与嵌套的对象按没写。LIB-SMALL 以前自写的解析器把嵌套块的
/// 原文当文本读，真的 YAML 解析之后嵌套的对象就是对象（ext.md §2.2）。
fn text<'a>(
    map: &'a std::collections::BTreeMap<String, Value>,
    key: &str,
) -> Option<std::borrow::Cow<'a, str>> {
    map.get(key)?.text()
}

/// 列表里的字符串项（别的类型的项不算，与参照的工具清单、`paths` 读法相同）。
fn strings(items: &[Value]) -> Vec<&str> {
    items.iter().filter_map(Value::as_str).collect()
}

/// 没有 `description` 时取正文第一个非空行；正文为空时是 `fallback`。
fn first_line(body: &str, fallback: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let heading = line
            .strip_prefix('#')
            .map(|rest| rest.trim_start_matches('#'))
            .filter(|rest| rest.starts_with([' ', '\t']))
            .map(str::trim)
            .filter(|rest| !rest.is_empty());
        let text = heading.unwrap_or(line);
        // 截断加省略号只定义在 base 的 `text`。
        return text::truncate_chars(text, MAX_DESCRIPTION_CHARS).into_owned();
    }
    fallback.to_owned()
}

/// 命名参数：去掉空串与纯数字。
fn arg_names(value: Option<&Value>) -> Vec<String> {
    let items: Vec<String> = match value {
        Some(Value::List(items)) => strings(items).into_iter().map(str::to_owned).collect(),
        Some(value) => value
            .text()
            .map(|text| text.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    items
        .into_iter()
        .filter(|item| !item.trim().is_empty() && !item.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

/// 字段的值；写了却空着（`null`）与没写相同。
fn present<'a>(map: &'a std::collections::BTreeMap<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).filter(|value| **value != Value::Null)
}

/// 工具清单字段（ext.md §2.2 的字段表）：没写是空清单。
fn tools(value: Option<&Value>) -> Vec<String> {
    value.map(frontmatter::tool_list).unwrap_or_default()
}

/// `shell`：`String(v)` 去首尾空白、转小写后是 `powershell` 时用 PowerShell，别的与没写都是 Bash。
fn shell(value: Option<&Value>) -> SkillShell {
    let shell = value.map(Value::js_string).unwrap_or_default();
    if rust_cli_base::js::string::trim(&shell).eq_ignore_ascii_case(POWER_SHELL) {
        SkillShell::PowerShell
    } else {
        SkillShell::Bash
    }
}

/// `shell` 写 PowerShell 时的取值（不分大小写）。
const POWER_SHELL: &str = "powershell";

/// 解析的规则：skill（§2.2）或旧 `commands/` 目录的命令（§2.10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flavor {
    Skill,
    /// 不读 `name`（命令没有显示名）与 `paths`（不是条件命令）；描述的回落是 [`COMMAND_DESCRIPTION`]。
    Command,
}

/// 一个 SKILL.md；没有 frontmatter 时整份都是正文。
pub(crate) fn parse(raw: &str, name: String, dir: &str, source: SkillSource) -> Skill {
    parse_as(raw, name, Some(dir), source, Flavor::Skill)
}

/// `dir` 是基础目录（`Base directory for this skill:` 与 `${CLAUDE_SKILL_DIR}`）；单个 `.md` 的命令没有（§2.10）。
pub(super) fn parse_as(
    raw: &str,
    name: String,
    dir: Option<&str>,
    source: SkillSource,
    flavor: Flavor,
) -> Skill {
    let frontmatter::Frontmatter {
        fields: map,
        body,
        error,
    } = frontmatter::parse(raw);
    if let Some(error) = error {
        tracing::warn!(skill = %name, %error, "the YAML frontmatter failed to parse and was ignored");
    }
    let fallback = match flavor {
        Flavor::Skill => tool_names::SKILL,
        Flavor::Command => COMMAND_DESCRIPTION,
    };
    let written = text(&map, "description")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let described = written.is_some();
    let description = written.unwrap_or_else(|| first_line(&body, fallback));
    let command = flavor == Flavor::Command;
    let model = text(&map, "model")
        .map(|model| model.trim().to_owned())
        .filter(|model| !model.is_empty() && !model.eq_ignore_ascii_case("inherit"));
    Skill {
        name,
        display: text(&map, "name").filter(|_| !command).map(Into::into),
        description,
        described,
        when_to_use: text(&map, "when_to_use").map(Into::into),
        argument_hint: text(&map, "argument-hint").map(Into::into),
        arg_names: arg_names(map.get("arguments")),
        allowed_tools: tools(map.get("allowed-tools")),
        // `disallowed-tools` 没写或空着时读 `disallowedTools`。
        disallowed_tools: tools(
            present(&map, "disallowed-tools").or_else(|| present(&map, "disallowedTools")),
        ),
        // `yes`、`on`、`1` 也是真（ext.md §2.2 的字段表）。修复：以前只认 `true`，写 `yes` 的 skill 进了清单、Skill 工具照常
        // 调用。
        disable_model_invocation: map
            .get("disable-model-invocation")
            .and_then(Value::flag)
            .unwrap_or(false),
        // 没写可以调用；写了只有认出的真值可以（空着、`maybe` 都不可以）。修复：以前空着按没写（可以调用）、`on` 按假。
        user_invocable: map
            .get("user-invocable")
            .is_none_or(|value| value.flag() == Some(true)),
        // 修复：以前只按逗号切，`src/*.{ts,tsx}` 切成两段坏的模式，条件 skill 不激活。
        paths: frontmatter::paths(map.get("paths")).filter(|_| !command),
        shell: shell(present(&map, "shell")),
        // 写了 `hooks:` 却空着（`null`）与没写相同（以前自写的解析器丢掉空着的键）。
        has_hooks: map.get("hooks").is_some_and(|hooks| *hooks != Value::Null),
        model,
        effort: text(&map, "effort").map(Into::into),
        fork: text(&map, "context").as_deref().map(str::trim) == Some("fork"),
        agent: text(&map, "agent").map(Into::into),
        dir: dir.map(str::to_owned),
        body,
        source,
        kind: match flavor {
            Flavor::Skill => UserCommandKind::Skill,
            Flavor::Command => UserCommandKind::Command,
        },
    }
}

#[cfg(test)]
#[path = "load_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "load_latency_tests.rs"]
mod latency_tests;
