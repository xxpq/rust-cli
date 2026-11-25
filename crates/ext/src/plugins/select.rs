//! 插件的来源与启用判定（docs/specs/rust-cli/ext.md §4.1、§4.3、§4.4）。
//!
//! 入口层给出候选目录（配置 `plugins.dirs`、内置插件、已安装插件，按 Node `resolveCandidates` 的顺序）、合并后的
//! `enabledPlugins`、`suppressedBuiltins`、官方表（数据）、配置值与数据根。这里读 manifest 得到插件 id，逐条判定，加载
//! 启用的插件。`--plugin-dir` 的插件（会话插件）排在最前，缺省启用，并盖过同名的插件。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use futures_util::future::join_all;
use rust_cli_base::plugin::{PluginContext, PluginOptions, sanitize_id};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::{EnsureDir, FileKind, PathStyle};
use rust_cli_kernel_api::machine::Files;
use serde_json::{Map, Value};

use super::components::{mark_sensitive, user_config};
use super::set::{PluginError, PluginErrorKind, PluginSet};
use super::{Manifest, Plugin, Reader, base_name};

/// Node `ZCODE_OFFICIAL_PLUGIN_MARKETPLACE`（定义在 base，F55）。
const OFFICIAL: &str = rust_cli_base::names::OFFICIAL_MARKETPLACE;
/// Node `ZCODE_INLINE_PLUGIN_MARKETPLACE`。
const INLINE: &str = rust_cli_base::names::INLINE_MARKETPLACE;
/// manifest 的缺省启用字段（只对 `inline` 的插件生效，§4.3 第 6 步）。
const DEFAULT_ENABLED_KEY: &str = "defaultEnabled";

/// 前端撑不起来的插件（context.md §15.7）：插件 id → 原因（日志里写它）。入口按前端声明的能力算好交来；ext 只按 id 查，
/// 不认识前端的能力。
pub type Unhosted = BTreeMap<String, String>;

/// 候选目录的来源（Node `PluginSource`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PluginOrigin {
    /// `--plugin-dir`（会话插件）。
    #[default]
    Session,
    /// 配置 `plugins.dirs`（Node 的 `inline`），缺省启用。
    ConfigDir,
    /// 内置插件（内置目录或 Node 播种的缓存，Node 的 `official`），`suppressedBuiltins` 只对它生效。
    Official,
    /// `installed_plugins.json` 里的插件（Node 的 `cache`）。
    Installed,
}

/// 入口层给出的一个候选目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCandidate {
    pub dir: EnvPath,
    /// 插件 id 的后半（`inline`、`zcode-plugins-official` 或记录里的 marketplace）。
    pub marketplace: String,
    pub origin: PluginOrigin,
}

/// 官方表（ext.md §4.3、§4.4）：入口层从数据文件读进来的 id 集合。内核里没有插件名。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfficialTable {
    /// 缺省启用的 id（Node `DEFAULT_ENABLED_OFFICIAL_PLUGIN_IDS`）。
    pub default_enabled: BTreeSet<String>,
    /// 能力来自 Node 宿主注入的 `node_repl` MCP 的 id。
    pub node_repl: BTreeSet<String>,
    /// MCP 要 Node 运行时的 id。
    pub node_runtime: BTreeSet<String>,
}

/// 入口层组装的插件来源（第二段的值，ext.md §4.0）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSources {
    /// 按 Node 的顺序：配置目录、内置插件、已安装插件。`plugins.enabled` 为 false 时为空。
    pub candidates: Vec<PluginCandidate>,
    /// 合并后的 `plugins.enabledPlugins`（插件 id → 是否启用）。
    pub enabled_plugins: BTreeMap<String, bool>,
    /// 合并后的 `plugins.suppressedBuiltins`。
    pub suppressed_builtins: Vec<String>,
    pub official: OfficialTable,
    /// 存下的配置值（插件 id → 键 → 值，已按层取好，§4.9）。
    pub options: BTreeMap<String, PluginOptions>,
    /// 数据根（执行侧的路径，§4.9）；None 时插件没有数据目录。
    pub data_root: Option<EnvPath>,
}

/// 官方插件依赖的 Node 运行时或宿主（ext.md §4.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeDependency {
    /// 能力来自 Node 宿主注入的 `node_repl` MCP。
    NodeRepl,
    /// MCP 要 Node 运行时（Node 经它自身的插件宿主模式启动）。
    PluginHost,
    /// MCP 要 Node 注入的官方鉴权。
    OfficialAuth,
}

impl NodeDependency {
    fn describe(self) -> &'static str {
        match self {
            Self::NodeRepl => "it relies on the node_repl MCP server injected by the Node host",
            Self::PluginHost => "its MCP server runs in the Node CLI plugin host",
            Self::OfficialAuth => {
                "its MCP server needs the official auth injected by the Node host"
            }
        }
    }
}

/// 候选目录没有加载的原因（ext.md §4.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    RootMissing,
    ManifestMissing,
    ManifestInvalid(String),
    /// 在 `suppressedBuiltins` 里的内置插件。
    Suppressed,
    /// 同一个 id 已经出现过（先到先得）。
    DuplicateId,
    /// 内置插件与用户从市场装的同 id 插件：装的那个排在前面、生效（A15，plugin-store.md §8.2）。
    InstalledOverride,
    Disabled,
    /// 与 `--plugin-dir` 的插件同名。
    ShadowedBySession,
    /// 与已经加载的插件同名（marketplace 不同，ext.md D22）。
    DuplicateName,
    NodeRuntime(NodeDependency),
    /// 前端撑不起来（[`Unhosted`]），带入口给的原因。
    FrontendLacks(String),
}

impl SkipReason {
    pub fn describe(&self) -> String {
        match self {
            Self::RootMissing => "the plugin root does not exist".to_owned(),
            Self::ManifestMissing => "the plugin manifest was not found".to_owned(),
            Self::ManifestInvalid(error) => format!("the plugin manifest is invalid: {error}"),
            Self::Suppressed => "the built-in plugin is suppressed".to_owned(),
            Self::DuplicateId => "duplicate plugin ignored".to_owned(),
            Self::InstalledOverride => {
                "an installed plugin with the same id takes precedence".to_owned()
            }
            Self::Disabled => "the plugin is not enabled".to_owned(),
            Self::ShadowedBySession => "a --plugin-dir plugin has the same name".to_owned(),
            Self::DuplicateName => "a loaded plugin has the same name".to_owned(),
            Self::NodeRuntime(dependency) => dependency.describe().to_owned(),
            Self::FrontendLacks(why) => why.clone(),
        }
    }

    /// 进加载错误表的原因（ext.md §4.14）；别的原因不是错误。
    fn error_kind(&self) -> Option<PluginErrorKind> {
        match self {
            Self::RootMissing => Some(PluginErrorKind::PathNotFound),
            Self::ManifestMissing | Self::ManifestInvalid(_) => {
                Some(PluginErrorKind::ManifestValidation)
            }
            _ => None,
        }
    }
}

/// 一个没有加载的候选目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub dir: String,
    /// manifest 读出之后才有。
    pub id: Option<String>,
    pub reason: SkipReason,
}

/// 插件名最多这么多个字节。
const MAX_NAME_LEN: usize = 128;

/// Node `PLUGIN_NAME_PATTERN`：`^[a-z0-9][a-z0-9._-]{0,127}$`。
fn valid_name(name: &str) -> bool {
    let lower_or_digit = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut chars = name.chars();
    chars.next().is_some_and(lower_or_digit)
        && name.len() <= MAX_NAME_LEN
        && chars.all(|c| lower_or_digit(c) || matches!(c, '.' | '_' | '-'))
}

/// Node `readManifest` 的名字规则与报错文字。
fn manifest_name(manifest: &Value) -> Result<String, String> {
    let Some(object) = manifest.as_object() else {
        return Err("Manifest must be a JSON object".to_owned());
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if valid_name(name) {
        Ok(name.to_owned())
    } else {
        Err(format!("Invalid plugin name: {name}"))
    }
}

/// 官方插件依赖 Node 运行时或宿主的判定（ext.md §4.4）；`table` 是 MCP 服务器声明。
fn node_dependency(
    id: &str,
    marketplace: &str,
    official: &OfficialTable,
    table: &Map<String, Value>,
) -> Option<NodeDependency> {
    if marketplace != OFFICIAL {
        return None;
    }
    if official.node_repl.contains(id) {
        return Some(NodeDependency::NodeRepl);
    }
    if official.node_runtime.contains(id) {
        return Some(NodeDependency::PluginHost);
    }
    if table.values().any(|server| server.get("auth").is_some()) {
        return Some(NodeDependency::OfficialAuth);
    }
    None
}

/// manifest 的 `defaultEnabled`（布尔时）。
fn manifest_default(manifest: &Value) -> Option<bool> {
    manifest.get(DEFAULT_ENABLED_KEY).and_then(Value::as_bool)
}

/// 两层都没写启停时的值（§4.3 第 5 步）：会话插件与配置目录的插件看 manifest 的 `defaultEnabled`（缺省开），内置与已安装的看
/// 官方表。插件管理列出「缺省启用」也用它，与加载同一条规则。
pub fn default_enabled(
    origin: PluginOrigin,
    id: &str,
    manifest: &Value,
    official: &OfficialTable,
) -> bool {
    match origin {
        PluginOrigin::Session | PluginOrigin::ConfigDir => {
            manifest_default(manifest).unwrap_or(true)
        }
        PluginOrigin::Official | PluginOrigin::Installed => official.default_enabled.contains(id),
    }
}

/// 一个目录判定之前读到的（ext.md §4.0 的加载分三段）：根的种类、manifest 与 MCP 表。全部目录同时读，读的时候不判定、
/// 不改状态。
struct Probe {
    is_dir: bool,
    manifest: Manifest,
    /// manifest 可用时（会话插件读不出 manifest 时按空的算）的 MCP 表（[`Reader::mcp_table`]）。
    table: Map<String, Value>,
}

impl Reader<'_> {
    async fn probe(&self, dir: &str) -> Probe {
        if self.kind(dir).await != Some(FileKind::Directory) {
            return Probe {
                is_dir: false,
                manifest: Manifest::Missing,
                table: Map::new(),
            };
        }
        let manifest = self.manifest(dir).await;
        let value = match &manifest {
            Manifest::Found(value) => value,
            Manifest::Missing | Manifest::Invalid(_) => &Value::Null,
        };
        let table = self.mcp_table(dir, value).await;
        Probe {
            is_dir: true,
            manifest,
            table,
        }
    }

    /// 数据目录（§4.9）：数据根下的净化 id，经主 env 建；建不了时记 warn，路径照样给。
    async fn data_dir(&self, data_root: Option<&EnvPath>, id: &str) -> Option<String> {
        let root = data_root?;
        let style = PathStyle::infer(root.as_str());
        let dir = style.join(root.as_str(), &[&sanitize_id(id)]);
        let request = EnsureDir {
            path: EnvPath::new(dir.as_str()),
        };
        if let Err(error) = self.files.ensure_dir(self.workspace, request).await {
            tracing::warn!(plugin = id, dir, %error, "the plugin data directory could not be created");
        }
        Some(dir)
    }

    /// 接纳一个插件：数据目录、配置值、组件。组件的错误随插件返回，由调用方按判定的顺序并入。
    async fn accept(
        &self,
        sources: &PluginSources,
        pending: Pending,
    ) -> (Plugin, Vec<PluginError>) {
        let Pending {
            dir,
            identity: (name, marketplace, origin),
            manifest,
            table,
        } = pending;
        let id = format!("{name}@{marketplace}");
        let options = sources.options.get(&id).cloned().unwrap_or_default();
        let context = PluginContext {
            data: self.data_dir(sources.data_root.as_ref(), &id).await,
            options: mark_sensitive(options, &manifest),
            declares_options: user_config(&manifest).is_some(),
            id,
            name,
            root: dir,
        };
        let mut errors = Vec::new();
        let components = self
            .components(&context, &manifest, table, &mut errors)
            .await;
        let plugin = Plugin {
            context: Arc::new(context),
            marketplace,
            origin,
            manifest,
            skill_dirs: components.skill_dirs,
            command_roots: components.command_roots,
            mcp_servers: components.mcp_servers,
            agent_files: components.agent_files,
            hooks: components.hooks,
        };
        (plugin, errors)
    }
}

/// 判定通过、等读组件的插件。
struct Pending {
    dir: String,
    identity: (String, String, PluginOrigin),
    manifest: Value,
    table: Map<String, Value>,
}

/// 跳过的目录（第三段记日志与错误）。
struct Skip {
    dir: String,
    id: Option<String>,
    reason: SkipReason,
    /// `--plugin-dir` 的序号标签（还没读出 id 时进错误表）。
    tag: Option<String>,
}

/// 一个目录的判定结果，按输入的顺序排（第三段按它并入，错误与跳过的顺序同以前逐个加载时）。
enum Outcome {
    Skip(Skip),
    Accept(Pending),
}

/// 第二段的状态：启用的会话插件的名字、已出现的 id、已接纳的名字与各目录的判定结果。只做判定，不读文件。
struct Admission<'a> {
    sources: &'a PluginSources,
    /// 前端撑不起来的插件（入口给的，context.md §15.7）。
    unhosted: &'a Unhosted,
    session: HashSet<String>,
    /// 已出现的 id 与它第一次出现时的来源。
    seen: HashMap<String, PluginOrigin>,
    accepted: HashSet<String>,
    outcomes: Vec<Outcome>,
}

impl Admission<'_> {
    fn skip(&mut self, dir: &str, id: Option<&str>, reason: SkipReason, tag: Option<String>) {
        self.outcomes.push(Outcome::Skip(Skip {
            dir: dir.to_owned(),
            id: id.map(str::to_owned),
            reason,
            tag,
        }));
    }

    fn accept(
        &mut self,
        dir: &str,
        identity: (String, String, PluginOrigin),
        manifest: Value,
        table: Map<String, Value>,
    ) {
        self.accepted.insert(identity.0.clone());
        self.outcomes.push(Outcome::Accept(Pending {
            dir: dir.to_owned(),
            identity,
            manifest,
            table,
        }));
    }

    /// `--plugin-dir` 的插件：manifest 缺失、读不出或没有 `name` 时插件名取目录名；`enabledPlugins["<name>@inline"]` 为
    /// false 或 manifest 的 `defaultEnabled: false` 时不加载（ext.md §4.3 第 6 步）。`index` 是它在命令行里的序号。
    fn session(&mut self, dir: &str, index: usize, probe: Probe) {
        if !probe.is_dir {
            self.skip(
                dir,
                None,
                SkipReason::RootMissing,
                Some(format!("{INLINE}[{index}]")),
            );
            return;
        }
        let manifest = match probe.manifest {
            Manifest::Found(value) => value,
            Manifest::Missing => Value::Null,
            Manifest::Invalid(error) => {
                tracing::warn!(dir, error, "plugin manifest is not valid JSON");
                Value::Null
            }
        };
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map_or_else(|| base_name(dir), str::to_owned);
        let id = format!("{name}@{INLINE}");
        let default = default_enabled(
            PluginOrigin::Session,
            &id,
            &manifest,
            &self.sources.official,
        );
        if !self
            .sources
            .enabled_plugins
            .get(&id)
            .copied()
            .unwrap_or(default)
        {
            self.skip(dir, Some(&id), SkipReason::Disabled, None);
            return;
        }
        // 会话插件不占 id：同名的配置目录插件按“被会话插件盖过”跳过（第 7 步）。
        self.session.insert(name.clone());
        let identity = (name, INLINE.to_owned(), PluginOrigin::Session);
        self.accept(dir, identity, manifest, probe.table);
    }

    /// 一个候选目录（ext.md §4.3 的十步）。
    fn candidate(&mut self, candidate: &PluginCandidate, probe: Probe) {
        let dir = candidate.dir.as_str();
        if !probe.is_dir {
            return self.skip(dir, None, SkipReason::RootMissing, None);
        }
        let manifest = match probe.manifest {
            Manifest::Found(value) => value,
            Manifest::Missing => return self.skip(dir, None, SkipReason::ManifestMissing, None),
            Manifest::Invalid(error) => {
                return self.skip(dir, None, SkipReason::ManifestInvalid(error), None);
            }
        };
        let name = match manifest_name(&manifest) {
            Ok(name) => name,
            Err(error) => return self.skip(dir, None, SkipReason::ManifestInvalid(error), None),
        };
        let id = format!("{name}@{}", candidate.marketplace);
        let reject = |this: &mut Self, reason| this.skip(dir, Some(&id), reason, None);
        if candidate.origin == PluginOrigin::Official
            && self.sources.suppressed_builtins.contains(&id)
        {
            return reject(self, SkipReason::Suppressed);
        }
        if let Some(first) = self.seen.get(&id) {
            let reason = if *first == PluginOrigin::Installed
                && candidate.origin == PluginOrigin::Official
            {
                SkipReason::InstalledOverride
            } else {
                SkipReason::DuplicateId
            };
            return reject(self, reason);
        }
        self.seen.insert(id.clone(), candidate.origin);
        let default = default_enabled(candidate.origin, &id, &manifest, &self.sources.official);
        if !self
            .sources
            .enabled_plugins
            .get(&id)
            .copied()
            .unwrap_or(default)
        {
            return reject(self, SkipReason::Disabled);
        }
        if self.session.contains(&name) {
            return reject(self, SkipReason::ShadowedBySession);
        }
        if self.accepted.contains(&name) {
            return reject(self, SkipReason::DuplicateName);
        }
        if let Some(why) = self.unhosted.get(&id) {
            return reject(self, SkipReason::FrontendLacks(why.clone()));
        }
        let official = &self.sources.official;
        if let Some(dependency) =
            node_dependency(&id, &candidate.marketplace, official, &probe.table)
        {
            return reject(self, SkipReason::NodeRuntime(dependency));
        }
        let identity = (name, candidate.marketplace.clone(), candidate.origin);
        self.accept(dir, identity, manifest, probe.table);
    }
}

/// 第三段的并入：跳过的记日志与错误（ext.md §4.14），接纳的插件带上它的组件错误。
#[derive(Default)]
struct Merged {
    plugins: Vec<Plugin>,
    skipped: Vec<Skipped>,
    errors: Vec<PluginError>,
}

impl Merged {
    fn skip(
        &mut self,
        Skip {
            dir,
            id,
            reason,
            tag,
        }: Skip,
    ) {
        let skipped = Skipped { dir, id, reason };
        log(&skipped);
        if let Some(kind) = skipped.reason.error_kind() {
            // 还没读出 id 时：`--plugin-dir` 写它的序号标签与目录（ext.md §4.14），别的写目录。
            let (plugin, path) = match (&skipped.id, tag) {
                (Some(id), _) => (id.clone(), None),
                (None, Some(tag)) => (tag, Some(skipped.dir.clone())),
                (None, None) => (skipped.dir.clone(), None),
            };
            self.errors.push(PluginError {
                plugin,
                kind,
                message: skipped.reason.describe(),
                path,
            });
        }
        self.skipped.push(skipped);
    }
}

fn log(skipped: &Skipped) {
    let (dir, id, reason) = (
        skipped.dir.as_str(),
        skipped.id.as_deref().unwrap_or(""),
        skipped.reason.describe(),
    );
    match skipped.reason {
        SkipReason::Disabled
        | SkipReason::Suppressed
        | SkipReason::ShadowedBySession
        | SkipReason::InstalledOverride => {
            tracing::debug!(dir, id, reason, "plugin skipped");
        }
        SkipReason::NodeRuntime(_) | SkipReason::FrontendLacks(_) => {
            tracing::info!(dir, id, reason, "plugin skipped");
        }
        _ => tracing::warn!(dir, id, reason, "plugin skipped"),
    }
}

/// 加载 `--plugin-dir` 的插件与入口层给出的候选目录（ext.md §4.3），得到这套内核的 [`PluginSet`]。`files` 是主 env 的端口；
/// `unhosted` 是前端撑不起来的插件（context.md §15.7）。
pub async fn load(
    files: &dyn Files,
    workspace: &WorkspaceRef,
    session_dirs: &[EnvPath],
    (sources, unhosted): (&PluginSources, &Unhosted),
) -> PluginSet {
    let reader = Reader { files, workspace };
    // 第一段：全部目录同时读（ext.md §4.0）。先收成 Vec 再 await（rust-lang/rust#102211）。
    let session_probes: Vec<_> = session_dirs
        .iter()
        .map(|dir| reader.probe(dir.as_str()))
        .collect();
    let candidate_probes: Vec<_> = sources
        .candidates
        .iter()
        .map(|candidate| reader.probe(candidate.dir.as_str()))
        .collect();
    let (session_probes, candidate_probes) =
        futures_util::join!(join_all(session_probes), join_all(candidate_probes));
    // 第二段：按原来的顺序逐个判定，不读文件。
    let mut admission = Admission {
        sources,
        unhosted,
        session: HashSet::new(),
        seen: HashMap::new(),
        accepted: HashSet::new(),
        outcomes: Vec::new(),
    };
    for (index, (dir, probe)) in session_dirs.iter().zip(session_probes).enumerate() {
        admission.session(dir.as_str(), index, probe);
    }
    for (candidate, probe) in sources.candidates.iter().zip(candidate_probes) {
        admission.candidate(candidate, probe);
    }
    // 第三段：接纳的插件同时读组件，结果按判定的顺序并入。
    // `order` 里 None 是一个接纳的插件，按顺序取 `accepted` 的下一项。
    let mut accepts = Vec::new();
    let mut order = Vec::new();
    for outcome in admission.outcomes {
        match outcome {
            Outcome::Accept(pending) => {
                accepts.push(reader.accept(sources, pending));
                order.push(None);
            }
            Outcome::Skip(skip) => order.push(Some(skip)),
        }
    }
    let mut accepted = join_all(accepts).await.into_iter();
    let mut merged = Merged::default();
    for slot in order {
        match slot {
            Some(skip) => merged.skip(skip),
            None => {
                if let Some((plugin, errors)) = accepted.next() {
                    merged.plugins.push(plugin);
                    merged.errors.extend(errors);
                }
            }
        }
    }
    let set = PluginSet::new(merged.plugins, merged.skipped, merged.errors);
    tracing::info!(
        event = "plugins.set.loaded",
        id = set.id(),
        plugins = set.plugins().len(),
        errors = set.errors().len(),
        "plugins loaded"
    );
    set
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "load_latency_tests.rs"]
mod latency_tests;
