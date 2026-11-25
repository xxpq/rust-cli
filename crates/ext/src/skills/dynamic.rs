//! 嵌套目录的 skill（docs/specs/rust-cli/ext.md §2.6.1，按对照实现）：历史里成功的 Read / Write /
//! Edit 触及 cwd 下面的文件时，从文件所在目录向上（严格在 cwd 下面）找 `.zcode/skills`、`.agents/skills`，找到的 skill 并进
//! 清单与 Skill 工具。和条件 skill 一样从历史推导（§2.6），不记进程状态。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures_util::future::join_all;
use rust_cli_base::names::{DOT_AGENTS_DIR, DOT_DIR};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, ListDir, PathStyle, StatMany};
use rust_cli_kernel_api::history::touched_files;
use rust_cli_kernel_api::listing::DynamicSkillDir;
use rust_cli_kernel_api::message::Message;

use super::load::{Candidate, MAX_SKILL_BYTES, Reader, SKILL_FILE, SKILLS_DIR, parse};
use super::{Skill, SkillCatalog, SkillSource};

/// 这一步从嵌套目录发现的：并进清单的 skill（已按 §2.6.1 的规则排好、改好名），与每个目录的宣布内容。
#[derive(Default)]
pub(crate) struct Dynamic {
    pub(crate) skills: Vec<Arc<Skill>>,
    pub(crate) dirs: Vec<DynamicSkillDir>,
}

/// 一个存在的 skill 目录：路径、前缀（父目录相对 cwd，`pkg`）、目录名（`.zcode/skills`）、显示路径与它的 skill（列目录的原始
/// 顺序）。
struct Found {
    dir: String,
    prefix: String,
    label: String,
    display: String,
    skills: Vec<Arc<Skill>>,
}

/// `base` 是这一步 §2.1 发现的全部 skill（含插件与内置）。`history` 是压缩边界之后的部分。
pub(crate) async fn discover(
    catalog: &SkillCatalog,
    workspace: &WorkspaceRef,
    facts: &EnvFacts,
    (history, base): (&[Message], &[Arc<Skill>]),
) -> Dynamic {
    let style = PathStyle::of_platform(&facts.platform);
    let cwd = facts.cwd.as_str();
    let candidates = candidate_dirs(style, cwd, history);
    if candidates.is_empty() {
        return Dynamic::default();
    }
    let reader = Reader::new(catalog, workspace, style);
    let request = StatMany {
        paths: candidates.iter().map(EnvPath::new).collect(),
    };
    let Ok(stats) = reader.files.stat_many(workspace, request).await else {
        return Dynamic::default();
    };
    // 先收成 Vec 再 await（rust-lang/rust#102211）。
    let loads: Vec<_> = candidates
        .iter()
        .zip(stats)
        .filter(|(_, stat)| matches!(stat, Ok(Some(stat)) if stat.kind == FileKind::Directory))
        .map(|(dir, _)| load(&reader, cwd, dir))
        .collect();
    let found: Vec<Found> = join_all(loads).await.into_iter().flatten().collect();
    merge(base, found)
}

/// 触及的文件所在目录向上、严格在 cwd 下面的每一层的 skill 目录：每个文件深的在前，跨文件按出现的顺序去重。
fn candidate_dirs(style: PathStyle, cwd: &str, history: &[Message]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for file in touched_files(history) {
        let mut dir = style.dirname(file);
        while under(style, cwd, &dir) {
            for dot in [DOT_DIR, DOT_AGENTS_DIR] {
                let skills = style.join(&dir, &[dot, SKILLS_DIR]);
                if seen.insert(skills.clone()) {
                    dirs.push(skills);
                }
            }
            let parent = style.dirname(&dir);
            if parent == dir {
                break;
            }
            dir = parent;
        }
    }
    dirs
}

/// 相对 cwd 的 `/` 路径；在 cwd 外面或就是 cwd 时为 None。
fn relative(style: PathStyle, cwd: &str, path: &str) -> Option<String> {
    let relative = style.relative(cwd, path)?.replace('\\', "/");
    (!relative.is_empty() && !relative.starts_with("..")).then_some(relative)
}

fn under(style: PathStyle, cwd: &str, dir: &str) -> bool {
    relative(style, cwd, dir).is_some()
}

/// 一个 skill 目录里的 skill，按列目录的原始顺序（对照实现不排序）。没有 skill 时为 None。
async fn load(reader: &Reader<'_>, cwd: &str, dir: &str) -> Option<Found> {
    let style = reader.style;
    let request = ListDir {
        path: EnvPath::new(dir),
    };
    let entries = reader
        .files
        .list_dir(reader.workspace, request)
        .await
        .ok()?;
    let candidates = entries
        .into_iter()
        .filter(|entry| entry.kind == FileKind::Directory)
        .map(|entry| {
            let skill_dir = style.join(dir, &[&entry.name]);
            let file = style.join(&skill_dir, &[SKILL_FILE]);
            Candidate::new(file, MAX_SKILL_BYTES, move |raw| {
                let mut skill = parse(raw, entry.name, &skill_dir, SkillSource::Project);
                // 动态目录里的 `paths` 不起作用：发现了就可用（同对照实现）。
                skill.paths = None;
                skill
            })
        })
        .collect();
    let skills = reader.parse_all(candidates).await;
    if skills.is_empty() {
        return None;
    }
    // 前缀：去掉最后的 `/<.zcode|.agents>/skills`，相对 cwd。
    let dot = style.dirname(dir);
    Some(Found {
        prefix: relative(style, cwd, &style.dirname(&dot))?,
        label: format!("{}/{SKILLS_DIR}", style.file_name(&dot)),
        display: relative(style, cwd, dir)?,
        dir: dir.to_owned(),
        skills,
    })
}

/// 与基础清单比对、改名（§2.6.1 的表），再按目录给出宣布的名字。
fn merge(base: &[Arc<Skill>], found: Vec<Found>) -> Dynamic {
    let base_names: HashSet<&str> = base.iter().map(|skill| skill.name.as_str()).collect();
    let base_dirs: HashSet<&str> = base
        .iter()
        .filter_map(|skill| skill.dir.as_deref())
        .collect();
    // 每个 skill 带上它所在目录的前缀与目录名（`.zcode/skills`）。
    let mut all: Vec<(&Found, &Arc<Skill>)> = found
        .iter()
        .flat_map(|found| found.skills.iter().map(move |skill| (found, skill)))
        .filter(|(_, skill)| {
            !skill
                .dir
                .as_deref()
                .is_some_and(|dir| base_dirs.contains(dir))
        })
        .collect();
    // 按原名，再按 目录\0名字（§2.6.1）。
    all.sort_by(|(left_dir, left), (right_dir, right)| {
        left.name.cmp(&right.name).then_with(|| {
            format!("{}\0{}", left_dir.dir, left.name)
                .cmp(&format!("{}\0{}", right_dir.dir, right.name))
        })
    });
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (_, skill) in &all {
        *counts.entry(skill.name.as_str()).or_default() += 1;
    }
    let mut taken: HashSet<String> = HashSet::new();
    // 原来的 skill 目录 → 合并之后的名字（宣布用）。
    let mut renamed: HashMap<String, String> = HashMap::new();
    let mut skills = Vec::new();
    for (found, skill) in all {
        let shadows = base_names.contains(skill.name.as_str());
        let twin = counts.get(skill.name.as_str()).copied().unwrap_or(0) > 1;
        let from = format!(
            " (from {}/{} — applies when working on files under {}/)",
            found.prefix, found.label, found.prefix
        );
        let (name, suffix) = if shadows || twin {
            let scoped = format!("{}:{}", found.prefix, skill.name);
            if base_names.contains(scoped.as_str()) || taken.contains(&scoped) {
                continue;
            }
            let suffix = if shadows {
                format!(
                    " (scoped to {}/ — use this instead of the unscoped \"{}\" skill when the files being changed are under {}/)",
                    found.prefix, skill.name, found.prefix
                )
            } else {
                from
            };
            (scoped, suffix)
        } else {
            (skill.name.clone(), from)
        };
        let mut merged = Skill::clone(skill);
        merged.name.clone_from(&name);
        merged.description.push_str(&suffix);
        if let Some(dir) = &skill.dir {
            renamed.insert(dir.clone(), name.clone());
        }
        taken.insert(name);
        skills.push(Arc::new(merged));
    }
    let dirs = found
        .iter()
        .filter_map(|found| {
            let names: Vec<String> = found
                .skills
                .iter()
                .filter_map(|skill| renamed.get(skill.dir.as_deref()?).cloned())
                .collect();
            (!names.is_empty()).then(|| DynamicSkillDir {
                dir: found.dir.clone(),
                display: found.display.clone(),
                names,
            })
        })
        .collect();
    Dynamic { skills, dirs }
}
