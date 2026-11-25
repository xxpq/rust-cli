//! skill（docs/specs/rust-cli/ext.md §2）：发现、frontmatter、清单、Skill 工具；旧的 `commands/` 目录的命令也是 skill（§2.10）。
//!
//! 文件经执行环境端口读；解析结果按 SKILL.md 的路径与 `FileVersion` 缓存，每一步重新列目录、
//! `stat` 每个 SKILL.md，版本变了才重读。有文件变化的订阅（[`FileWatch`]，§2.1.1）时，发现的结果记下来，目录有变化才重新
//! 发现（对照实现的文件监视 + memoize）。

mod args;
mod body;
mod builtin;
mod closest;
mod commands;
mod dynamic;
mod inline;
mod invoked;
mod listing;
mod load;
mod mentions;
mod names;
mod paths;
mod tool;
mod unknown;
mod user;

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, FileVersion, WorkspaceRef};
use rust_cli_kernel_api::file_watch::{FileWatch, WatchRequest};
use rust_cli_kernel_api::listing::{DynamicSkillDir, ListingEntry};
use rust_cli_kernel_api::machine::{EnvInfo, Files};
use rust_cli_kernel_api::message::Message;

pub use invoked::INVOKED_SKILLS_SOURCE;
pub use tool::{SKILL_MESSAGE_SOURCE, SkillTool};
pub use user::SkillCommands;

/// `!` 命令的执行者：注册表里的 Bash 工具、判决链与 ID（ext.md §2.5）。
pub(crate) fn runner(
    bash: Arc<dyn rust_cli_kernel_api::tool::Tool>,
    gate: Arc<dyn rust_cli_kernel_api::orchestration::ScopedGate>,
    ids: Arc<dyn rust_cli_kernel_api::clock::Ids>,
) -> inline::Runner {
    inline::Runner { bash, gate, ids }
}

/// skill 从哪里来（`source`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    Managed,
    User,
    Project,
    Plugin {
        plugin: String,
    },
    /// 内置（bundled）：只有 `/loop`（tools-more.md §17.1）。
    Builtin,
}

/// `!` 命令用的 shell（frontmatter `shell`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillShell {
    #[default]
    Bash,
    PowerShell,
}

/// 一个 skill（解析结果里本工程用到的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// 调用名：目录名；插件 skill 为 `<插件名>:<目录名>`。
    pub name: String,
    /// frontmatter 的 `name`（面向用户的名字）。
    pub display: Option<String>,
    pub description: String,
    /// `description` 是写出来的（不是取正文第一行）：插件的 skill 与命令要写了它或 `when_to_use` 才进清单（ext.md §4.5.1）。
    pub described: bool,
    pub when_to_use: Option<String>,
    pub argument_hint: Option<String>,
    pub arg_names: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    pub paths: Option<Vec<String>>,
    pub shell: SkillShell,
    pub has_hooks: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fork: bool,
    pub agent: Option<String>,
    /// 基础目录（执行环境里的路径）：skill 的目录、`commands/` 里 `SKILL.md` 所在的目录。单个 `.md` 的命令与内置没有
    /// （ext.md §2.10：正文前不加 `Base directory for this skill:`，`${CLAUDE_SKILL_DIR}` 不替换）。
    pub dir: Option<String>,
    /// frontmatter 之后的正文，原样。
    pub body: String,
    pub source: SkillSource,
    /// 从哪类目录来（`skills/` 是 skill，`commands/` 与内置是命令）：只有加载时知道，`source` 只说层级（ext.md §2.7）。
    pub kind: rust_cli_kernel_api::user_command::UserCommandKind,
}

impl Skill {
    /// 只有安全字段：没有 `allowed-tools`、`disallowed-tools`、`hooks`。这样的 skill
    /// 不经询问即可调用（ext.md §2.4）。
    pub fn is_safe(&self) -> bool {
        // 内置的 `/loop` 不算安全：bundled skill 带 `argsMayContainSlashCommands` 等不在安全名单里的
        // 字段，所以要询问，dontAsk 下拒绝。
        self.source != SkillSource::Builtin
            && self.allowed_tools.is_empty()
            && self.disallowed_tools.is_empty()
            && !self.has_hooks
    }
}

/// 进 skill 清单：插件的 skill 与命令要写了 `description` 或 `when_to_use`（参照的规则：文件 skill、旧 `commands/` 目录与内置的
/// 没写时取正文第一行也进，插件的不进）。没进清单的照样能用 Skill 工具调用。
///
/// PLUG-P0 补的：以前插件 skill 一律进清单；插件命令是这条线加的，没写描述的命令在参照里不进清单。
fn listed(skill: &Skill) -> bool {
    !matches!(skill.source, SkillSource::Plugin { .. })
        || skill.described
        || skill.when_to_use.is_some()
}

/// 插件提供的 skill 根目录（ext.md §4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSkillRoot {
    pub plugin: String,
    /// `<插件>/skills` 一类的目录。
    pub dir: String,
}

#[derive(Default)]
struct Cache {
    /// SKILL.md 路径 → 解析时的版本与结果。
    parsed: HashMap<String, (FileVersion, Arc<Skill>)>,
}

/// 记下的发现结果（ext.md §2.1.1）：同一个工作区、cwd 与附加目录，订阅的流里没有变化时直接用。
struct Memo {
    key: MemoKey,
    found: Vec<Arc<Skill>>,
    changes: BoxStream<'static, ()>,
}

#[derive(PartialEq, Eq)]
struct MemoKey {
    workspace: WorkspaceRef,
    cwd: EnvPath,
    additional: Vec<EnvPath>,
}

/// 查记下的结果：可以直接用、要重新发现（带着原来的订阅），或者没有。
enum Lookup {
    Fresh(Vec<Arc<Skill>>),
    Stale(BoxStream<'static, ()>),
    Miss,
}

/// skill 目录的发现与缓存。
pub struct SkillCatalog {
    /// 环境事实（skill 目录的层级、路径风格）与 skill 文件；都是本机的。
    info: Arc<dyn EnvInfo>,
    files: Arc<dyn Files>,
    /// 这套内核的插件（ext.md §4.0）：skill 根、命令根与正文里的插件变量都从它来，构造之后不变。
    plugins: Arc<crate::plugins::PluginSet>,
    cache: Mutex<Cache>,
    /// 单次的 `-p`（`singleShotPrintSession`）：内置 `/loop` 的 Monitor 超时随它（tools-more.md §17.1）。
    print: AtomicBool,
    /// 文件变化的订阅（§2.1.1）：全局 watcher 的接口，现在没有实现（入口给 None），每一步照旧 stat。
    watch: Option<Arc<dyn FileWatch>>,
    memo: Mutex<Option<Memo>>,
}

impl SkillCatalog {
    pub fn new(info: Arc<dyn EnvInfo>, files: Arc<dyn Files>) -> Self {
        Self {
            info,
            files,
            plugins: Arc::default(),
            cache: Mutex::default(),
            print: AtomicBool::new(false),
            watch: None,
            memo: Mutex::default(),
        }
    }

    /// 带上文件变化的订阅（§2.1.1；构造时一次给）。
    pub fn with_watch(self, watch: Option<Arc<dyn FileWatch>>) -> Self {
        Self { watch, ..self }
    }

    /// 内核按形态设置：`-p` 是单次的打印会话。
    pub fn set_print_session(&self, print: bool) {
        self.print.store(print, Ordering::Relaxed);
    }

    pub(crate) fn print_session(&self) -> bool {
        self.print.load(Ordering::Relaxed)
    }

    /// 带上这套内核的插件（ext.md §4.0；构造时一次给）。
    pub fn with_plugins(self, plugins: Arc<crate::plugins::PluginSet>) -> Self {
        Self { plugins, ..self }
    }

    pub(crate) fn plugins(&self) -> &crate::plugins::PluginSet {
        &self.plugins
    }

    pub(crate) fn cached(&self, path: &str, version: &FileVersion) -> Option<Arc<Skill>> {
        let cache = self.cache.lock_unpoisoned();
        cache
            .parsed
            .get(path)
            .filter(|(known, _)| known == version)
            .map(|(_, skill)| skill.clone())
    }

    pub(crate) fn remember(&self, path: String, version: FileVersion, skill: Arc<Skill>) {
        let mut cache = self.cache.lock_unpoisoned();
        cache.parsed.insert(path, (version, skill));
    }

    /// 这一步发现的全部 skill，按优先级排好、同名只留第一个（ext.md §2.1）。`additional` 是会话的附加工作目录
    /// （session-dirs.md §3.3）。
    pub async fn skills(
        &self,
        workspace: &WorkspaceRef,
        additional: &[EnvPath],
    ) -> Vec<Arc<Skill>> {
        let facts = match self.info.facts(workspace).await {
            Ok(facts) => facts,
            Err(error) => {
                tracing::debug!(%error, "skills are not available without environment facts");
                return Vec::new();
            }
        };
        self.skills_with(workspace, &facts, additional).await
    }

    async fn skills_with(
        &self,
        workspace: &WorkspaceRef,
        facts: &EnvFacts,
        additional: &[EnvPath],
    ) -> Vec<Arc<Skill>> {
        let key = MemoKey {
            workspace: workspace.clone(),
            cwd: facts.cwd.clone(),
            additional: additional.to_vec(),
        };
        let mut found = match self.lookup(&key) {
            Lookup::Fresh(found) => found,
            Lookup::Stale(changes) => self.rediscover(key, facts, Some(changes)).await,
            Lookup::Miss => self.rediscover(key, facts, None).await,
        };
        // 内置 skill 排在文件与插件 skill 之后，同名时前面的生效。
        found.extend(builtin::skills());
        let mut seen = HashSet::new();
        found
            .into_iter()
            .filter(|skill| seen.insert(skill.name.clone()))
            .collect()
    }

    /// 记下的结果（§2.1.1）：key 相同时取出订阅里攒下的变化；有变化就要重新发现，流结束（订阅失效）就丢掉。取出来重新发现
    /// 时 memo 先清空，并发的另一次发现按没有算（各自订阅，后写的留下）。
    fn lookup(&self, key: &MemoKey) -> Lookup {
        let mut memo = self.memo.lock_unpoisoned();
        let Some(current) = memo.as_mut().filter(|memo| memo.key == *key) else {
            return Lookup::Miss;
        };
        let mut changed = false;
        // 只取已经到了的（`now_or_never` 不等）。
        while let Some(item) = current.changes.next().now_or_never() {
            match item {
                Some(()) => changed = true,
                None => {
                    *memo = None;
                    return Lookup::Miss;
                }
            }
        }
        if !changed {
            return Lookup::Fresh(current.found.clone());
        }
        match memo.take() {
            Some(memo) => Lookup::Stale(memo.changes),
            None => Lookup::Miss,
        }
    }

    /// 发现一次。先订阅再读（读的期间的变化留在流里，下一步再发现一次）；没有订阅时不记，下一步照旧重新发现。
    async fn rediscover(
        &self,
        key: MemoKey,
        facts: &EnvFacts,
        changes: Option<BoxStream<'static, ()>>,
    ) -> Vec<Arc<Skill>> {
        let layout = load::layout(self, &key.workspace, (facts, &key.additional)).await;
        let changes = changes.or_else(|| {
            let watch = self.watch.as_ref()?;
            let request = WatchRequest {
                dirs: layout.watched.clone(),
            };
            watch.subscribe(&key.workspace, request)
        });
        let found = load::discover(self, &key.workspace, (facts, &key.additional), &layout).await;
        if let Some(changes) = changes {
            *self.memo.lock_unpoisoned() = Some(Memo {
                key,
                found: found.clone(),
                changes,
            });
        }
        found
    }

    /// 这次会话调用过的 skill（compact.md §3.4）：`history` 是全部历史，最近的在前。
    pub async fn invoked(
        &self,
        (workspace, additional): (&WorkspaceRef, &[EnvPath]),
        history: &[Message],
    ) -> Vec<rust_cli_kernel_api::listing::InvokedSkill> {
        invoked::invoked(self, (workspace, additional), history).await
    }

    /// 这一步看得到的全部 skill：§2.1 的发现，加上 `history` 里（只看压缩边界之后）触及的嵌套目录的 skill（§2.6.1）。
    pub async fn skills_seen(
        &self,
        (workspace, additional): (&WorkspaceRef, &[EnvPath]),
        history: &[Message],
    ) -> Vec<Arc<Skill>> {
        let Ok(facts) = self.info.facts(workspace).await else {
            return Vec::new();
        };
        self.seen_with(workspace, &facts, additional, history)
            .await
            .0
    }

    /// 基础清单并上嵌套目录的 skill：动态的一块排在内置命令（`init` 等）之前（§2.6.1）；另给每个目录的宣布内容。
    async fn seen_with(
        &self,
        workspace: &WorkspaceRef,
        facts: &EnvFacts,
        additional: &[EnvPath],
        history: &[Message],
    ) -> (Vec<Arc<Skill>>, Vec<DynamicSkillDir>) {
        let mut skills = self.skills_with(workspace, facts, additional).await;
        // 只看最近一次压缩边界之后（同条件 skill，§2.6、D6）；调用方给全部历史或已切好的都可以。
        let window = rust_cli_kernel_api::history::since_boundary(history);
        let found = dynamic::discover(self, workspace, facts, (window, &skills)).await;
        if found.skills.is_empty() {
            return (skills, found.dirs);
        }
        let at = skills
            .iter()
            .position(|skill| builtin::is_command(skill))
            .unwrap_or(skills.len());
        skills.splice(at..at, found.skills);
        (skills, found.dirs)
    }

    /// 按名字找（Skill 工具用）。`history` 给嵌套目录的 skill（§2.6.1）。
    pub async fn find(
        &self,
        place: (&WorkspaceRef, &[EnvPath]),
        history: &[Message],
        name: &str,
    ) -> Option<Arc<Skill>> {
        self.skills_seen(place, history)
            .await
            .into_iter()
            .find(|skill| skill.name == name)
    }

    /// 这一步给模型的 skill 清单（ext.md §2.3）：去掉禁止模型调用的与未激活的条件 skill。预算按 `model` 的上下文
    /// 窗口（模型卡片，没有时 200000）与每 token 字符数（没有模型时 4）。另给嵌套目录里新发现的目录（§2.6.1）。
    pub async fn listing(
        &self,
        (workspace, additional): (&WorkspaceRef, &[EnvPath]),
        history: &[Message],
        model: &str,
    ) -> SkillListing {
        let Ok(facts) = self.info.facts(workspace).await else {
            return SkillListing::default();
        };
        let (skills, dynamic) = self.seen_with(workspace, &facts, additional, history).await;
        let touched = paths::touched(history, facts.cwd.as_str());
        let visible: Vec<Arc<Skill>> = skills
            .iter()
            .filter(|skill| !skill.disable_model_invocation)
            .filter(|skill| paths::active(skill, &touched))
            .filter(|skill| listed(skill))
            .cloned()
            .collect();
        let window = rust_cli_prompts::models::card(model).map(|card| card.context_window);
        let chars = rust_cli_prompts::models::chars_per_token(model);
        SkillListing {
            entries: listing::entries(&visible, &skills, listing::budget(window, chars)),
            dynamic,
        }
    }
}

/// 一步的 skill 清单与嵌套目录里发现的目录（§2.3、§2.6.1）。
#[derive(Debug, Default)]
pub struct SkillListing {
    pub entries: Vec<ListingEntry>,
    pub dynamic: Vec<DynamicSkillDir>,
}

/// 工具上下文看到的历史：主循环给这一步发给模型的消息；不经主循环的调用（斜杠命令等）问会话（没有时为空）。
pub(crate) async fn history_of(
    context: &rust_cli_kernel_api::tool::ToolUseContext,
) -> Arc<[Message]> {
    match &context.transcript {
        Some(transcript) => transcript.history.clone(),
        None => context
            .session_ops
            .conversation()
            .await
            .map(Arc::from)
            .unwrap_or_else(|| Arc::from(Vec::new())),
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "commands_tests.rs"]
mod commands_tests;

#[cfg(test)]
#[path = "watch_tests.rs"]
mod watch_tests;

#[cfg(test)]
#[path = "dynamic_tests.rs"]
mod dynamic_tests;
