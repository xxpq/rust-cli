//! 旧的 `commands/` 目录（docs/specs/rust-cli/ext.md §2.10）：managed、用户、项目每层与附加目录的 `.zcode/commands`、`.agents/commands`
//! 里的 `.md` 文件是 prompt 命令，与 skill 同一个结构、同一套展开。名字是相对根目录的路径，子目录用 `:` 连；一个目录里有
//! `SKILL.md` 时这个目录只取它（它有基础目录，单个 `.md` 没有）。收齐之后整体按名字排。

use std::sync::Arc;

use futures_util::future::join_all;
use rust_cli_base::names::{DOT_AGENTS_DIR, DOT_DIR, managed_dir};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::env_files::{FileKind, ListDir};

use super::load::{Candidate, Flavor, Reader, expand_allowed_tools, parse_as};
use super::{Skill, SkillSource};
use crate::plugins::{CommandMetadata, Plugin, PluginCommandRoot};

/// 每个根目录下的命令目录（`<根>/.zcode/commands`、`<根>/.agents/commands`）。
pub(super) const COMMANDS_DIR: &str = "commands";
const MARKDOWN_SUFFIX: &str = ".md";
/// 目录里有它（不分大小写）时，这个目录只取它（§2.10）。
const SKILL_FILE: &str = "skill.md";
/// 命令文件的大小上限（对照实现读 markdown 配置文件的上限），超过时跳过。
const MAX_COMMAND_BYTES: u64 = 1_048_576;
/// 递归的层数上限（根目录是第 0 层）：端口没有 inode，按层数防住循环的链接（D40，同 Node 的 `MAX_SCAN_DEPTH`）。
const MAX_DEPTH: usize = 12;

/// 一个命令目录与它的来源。
struct Root {
    dir: String,
    source: SkillSource,
}

/// 一个根目录里找到的命令文件：路径、相对根目录的目录段、是不是 `SKILL.md`。
struct Found {
    file: String,
    segments: Vec<String>,
    skill_format: bool,
}

/// 全部命令，按名字排好（同名的按收的顺序：managed、用户、近的项目层在前），未去重。`levels` 是 skill 发现算好的项目层级。
/// 各个根目录同时读（§2.1 末条）。
pub(super) async fn discover(
    reader: &Reader<'_>,
    facts: &EnvFacts,
    levels: &[String],
    additional: &[EnvPath],
) -> Vec<Arc<Skill>> {
    let style = reader.style;
    let bases = |base: &str, source: SkillSource| {
        // `.zcode` 与 `.agents` 合并，同级 `.zcode` 在前（Node `commandRootsForBase`）
        [DOT_DIR, DOT_AGENTS_DIR].map(|dot| Root {
            dir: style.join(base, &[dot, COMMANDS_DIR]),
            source: source.clone(),
        })
    };
    let mut roots = vec![Root {
        dir: style.join(managed_dir(&facts.platform), &[DOT_DIR, COMMANDS_DIR]),
        source: SkillSource::Managed,
    }];
    if let Some(home) = &facts.home {
        roots.extend(bases(home.as_str(), SkillSource::User));
    }
    for level in levels {
        roots.extend(bases(level, SkillSource::Project));
    }
    for dir in additional {
        roots.extend(bases(dir.as_str(), SkillSource::Project));
    }
    // 先收成 Vec 再 await（rust-lang/rust#102211，见 load.rs 的 discover）。
    let loads: Vec<_> = roots.iter().map(|root| load(reader, root)).collect();
    let mut commands: Vec<Arc<Skill>> = join_all(loads).await.into_iter().flatten().collect();
    // 稳定排序：同名的保持收的顺序（对照实现 `localeCompare`，这里同 skill 不分大小写，§2.1）
    commands.sort_by_key(|skill| (skill.name.to_lowercase(), skill.name.clone()));
    commands
}

/// 一个插件的命令（ext.md §4.5.1）：名字带插件前缀；按命令根的顺序，目录里按 [`walk`] 的顺序。`allowed-tools` 里的插件
/// 变量换掉，对象映射的字段盖过 frontmatter。各个命令根同时读（§2.1 末条）。
pub(super) async fn plugin(reader: &Reader<'_>, plugin: &Plugin) -> Vec<Arc<Skill>> {
    let source = SkillSource::Plugin {
        plugin: plugin.name().to_owned(),
    };
    let mut roots = Vec::new();
    for root in &plugin.command_roots {
        let source = source.clone();
        roots.push(async move {
            match root {
                PluginCommandRoot::Dir(dir) => {
                    let root = Root {
                        dir: dir.clone(),
                        source,
                    };
                    load_with(reader, &root, Some(plugin)).await
                }
                PluginCommandRoot::File {
                    path,
                    name,
                    metadata,
                } => {
                    let stem = reader.style.file_name(path);
                    let stem = stem.strip_suffix(MARKDOWN_SUFFIX).unwrap_or(&stem);
                    let name = format!("{}:{}", plugin.name(), name.as_deref().unwrap_or(stem));
                    let candidate = Candidate::new(path.clone(), MAX_COMMAND_BYTES, move |raw| {
                        let mut skill = parse_as(raw, name, None, source, Flavor::Command);
                        apply(&mut skill, metadata);
                        expand_allowed_tools(&mut skill, plugin);
                        skill
                    });
                    reader.parse_all(vec![candidate]).await
                }
            }
        });
    }
    join_all(roots).await.into_iter().flatten().collect()
}

/// 对象映射的字段盖过 frontmatter（§4.5.1）。
fn apply(skill: &mut Skill, metadata: &CommandMetadata) {
    if let Some(description) = &metadata.description {
        skill.description.clone_from(description);
        skill.described = true;
    }
    if let Some(hint) = &metadata.argument_hint {
        skill.argument_hint = Some(hint.clone());
    }
    if let Some(model) = &metadata.model {
        skill.model = Some(model.clone());
    }
    if let Some(tools) = &metadata.allowed_tools {
        skill.allowed_tools.clone_from(tools);
    }
}

/// 一个根目录里的命令。
async fn load(reader: &Reader<'_>, root: &Root) -> Vec<Arc<Skill>> {
    load_with(reader, root, None).await
}

/// 一个根目录里的命令；插件的命令名字带插件前缀，`allowed-tools` 换插件变量。
async fn load_with(reader: &Reader<'_>, root: &Root, plugin: Option<&Plugin>) -> Vec<Arc<Skill>> {
    let found = walk(reader, &root.dir, Vec::new()).await;
    let root_name = reader.style.file_name(&root.dir);
    let mut candidates = Vec::new();
    for item in found {
        let name = if item.skill_format {
            // 所在目录（连同上层的命名空间）；根目录下的取根目录的名字
            if item.segments.is_empty() {
                root_name.clone()
            } else {
                item.segments.join(":")
            }
        } else {
            let file_name = reader.style.file_name(&item.file);
            let stem = file_name
                .strip_suffix(MARKDOWN_SUFFIX)
                .unwrap_or(&file_name);
            item.segments
                .iter()
                .map(String::as_str)
                .chain([stem])
                .collect::<Vec<_>>()
                .join(":")
        };
        let name = match plugin {
            Some(plugin) => format!("{}:{name}", plugin.name()),
            None => name,
        };
        let dir = item.skill_format.then(|| reader.style.dirname(&item.file));
        let source = root.source.clone();
        candidates.push(Candidate::new(item.file, MAX_COMMAND_BYTES, move |raw| {
            let mut skill = parse_as(raw, name, dir.as_deref(), source, Flavor::Command);
            if let Some(plugin) = plugin {
                expand_allowed_tools(&mut skill, plugin);
            }
            skill
        }));
    }
    reader.parse_all(candidates).await
}

/// 递归找 `.md`：一个目录里有 `SKILL.md` 时只取它，子目录照常找（§2.10）。先是本目录的文件，再按名字是各个子目录的；
/// 子目录同时读（§2.1 末条）。
async fn walk(reader: &Reader<'_>, dir: &str, segments: Vec<String>) -> Vec<Found> {
    if segments.len() > MAX_DEPTH {
        return Vec::new();
    }
    let request = ListDir {
        path: EnvPath::new(dir),
    };
    let Ok(mut entries) = reader.files.list_dir(reader.workspace, request).await else {
        return Vec::new();
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let files: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.kind == FileKind::File && entry.name.ends_with(MARKDOWN_SUFFIX))
        .map(|entry| entry.name.as_str())
        .collect();
    let skill = files
        .iter()
        .copied()
        .find(|name| name.eq_ignore_ascii_case(SKILL_FILE));
    let picked = match skill {
        Some(skill) => vec![skill],
        None => files,
    };
    let mut found: Vec<Found> = picked
        .into_iter()
        .map(|name| Found {
            file: reader.style.join(dir, &[name]),
            segments: segments.clone(),
            skill_format: skill.is_some(),
        })
        .collect();
    let mut nested = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == FileKind::Directory)
    {
        let mut segments = segments.clone();
        segments.push(entry.name.clone());
        let child = reader.style.join(dir, &[&entry.name]);
        nested.push(Box::pin(
            async move { walk(reader, &child, segments).await },
        ));
    }
    found.extend(join_all(nested).await.into_iter().flatten());
    found
}
