//! 列出（plugin-store.md §13.2）：加载结果（ext 的 `load`，与会话加载同一套判定）里加载好的插件（启用）与判定为「未启用」
//! 的候选（停用）；依赖 Node 运行时、缺前端能力没有加载的也列出，诊断里写为什么。内置插件的 id 另由内置插件根的 manifest 算
//! （卸载与恢复内置用，§8.1）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rust_cli_ext::plugins::{Plugin, PluginErrorKind, PluginOrigin, SkipReason, load};
use rust_cli_plugin_store::core::diagnostic::{Diagnostic, StoreError, code};
use rust_cli_plugin_store::core::id::OFFICIAL_MARKETPLACE;
use rust_cli_plugin_store::core::manifest::{self, MANIFEST_PATHS};
use serde_json::Value;

use super::{Context, PluginAdmin};
use crate::assemble::env_layout::EnvLayout;
use crate::config::plugins::sources;

/// 一个 hook 的拍平（Node `PluginHookDetail`）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HookDetail {
    pub(crate) event: String,
    pub(crate) hook: serde_json::Map<String, Value>,
    pub(crate) matcher: Option<String>,
}

/// 列出的一个插件。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Listed {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) marketplace: String,
    /// `official`、`cache`、`inline`（Node `PluginSource`）。
    pub(crate) source: &'static str,
    pub(crate) root: String,
    pub(crate) manifest_path: String,
    pub(crate) enabled: bool,
    pub(crate) description: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) data_path: String,
    pub(crate) skill_roots: usize,
    pub(crate) skills: usize,
    pub(crate) command_roots: usize,
    pub(crate) declared_mcp: Vec<String>,
    pub(crate) mcp: Vec<String>,
    pub(crate) hooks: Vec<HookDetail>,
    /// manifest 的 `userConfig`（配置值判敏感键用）。
    pub(crate) user_config: Option<Value>,
}

/// 一个内置插件：根与 manifest。
#[derive(Debug, Clone)]
pub(crate) struct Builtin {
    pub(crate) root: PathBuf,
    pub(crate) manifest: Value,
}

/// 列出的结果：插件与诊断（加载的错误表，plugin_id 是插件 id 或候选目录）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Listing {
    pub(crate) plugins: Vec<Listed>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Listing {
    /// 按 id 或唯一的名字找（Node `resolvePluginSelector`）。找不到是 `PluginNotFound`（文字同 Node），名字不唯一是 `Invalid`。
    pub(crate) fn select(&self, selector: &str) -> Result<&Listed, StoreError> {
        let selector = selector.trim();
        if let Some(plugin) = self.plugins.iter().find(|p| p.id == selector) {
            return Ok(plugin);
        }
        let named: Vec<&Listed> = self.plugins.iter().filter(|p| p.name == selector).collect();
        match named.as_slice() {
            [one] => Ok(one),
            [] => Err(StoreError::PluginNotFound(selector.to_owned())),
            _ => Err(StoreError::Invalid(format!(
                "Plugin name is ambiguous, use full plugin id: {selector}"
            ))),
        }
    }
}

pub(super) fn origin_source(origin: PluginOrigin) -> &'static str {
    match origin {
        PluginOrigin::Official => "official",
        PluginOrigin::Installed => "cache",
        PluginOrigin::Session | PluginOrigin::ConfigDir => "inline",
    }
}

async fn manifest_path(root: &str) -> String {
    for path in MANIFEST_PATHS {
        let file = Path::new(root).join(path);
        if rust_cli_fs::is_file(&file).await {
            return file.display().to_string();
        }
    }
    String::new()
}

/// skill 的个数：一个根自己有 `SKILL.md` 时是一个，否则数带 `SKILL.md` 的子目录（ext.md §4.5）。
async fn count_skills(dirs: &[String]) -> usize {
    let mut count = 0;
    for dir in dirs {
        let dir = Path::new(dir);
        if rust_cli_fs::is_file(&dir.join(SKILL_FILE)).await {
            count += 1;
            continue;
        }
        for child in rust_cli_fs::subdirectories(dir).await.unwrap_or_default() {
            if rust_cli_fs::is_file(&dir.join(child).join(SKILL_FILE)).await {
                count += 1;
            }
        }
    }
    count
}

/// skill 的入口文件名。
pub(super) const SKILL_FILE: &str = "SKILL.md";

/// manifest 与 `.mcp.json` 声明的服务器名（加载结果里的服务器名是 `plugin:<插件>:<server>`）。
pub(super) fn declared_mcp(plugin: &Plugin) -> Vec<String> {
    let prefix = format!("plugin:{}:", plugin.name());
    plugin
        .mcp_servers
        .iter()
        .map(|server| {
            server
                .name
                .strip_prefix(&prefix)
                .unwrap_or(&server.name)
                .to_owned()
        })
        .collect()
}

/// 插件 hooks（`{"hooks": {事件: [{matcher?, hooks: [...]}]}}`）拍平。
pub(super) fn hook_details(plugin: &Plugin) -> Vec<HookDetail> {
    let mut found = Vec::new();
    for file in &plugin.hooks {
        let Some(events) = file.get("hooks").and_then(Value::as_object) else {
            continue;
        };
        for (event, groups) in events {
            for group in groups.as_array().into_iter().flatten() {
                let matcher = group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                for hook in group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(hook) = hook.as_object() {
                        found.push(HookDetail {
                            event: event.clone(),
                            hook: hook.clone(),
                            matcher: matcher.clone(),
                        });
                    }
                }
            }
        }
    }
    found
}

async fn listed(plugin: &Plugin) -> Listed {
    let text = |key: &str| {
        plugin
            .manifest
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Listed {
        id: plugin.id().to_owned(),
        name: plugin.name().to_owned(),
        marketplace: plugin.marketplace.clone(),
        source: origin_source(plugin.origin),
        root: plugin.dir().to_owned(),
        manifest_path: manifest_path(plugin.dir()).await,
        enabled: true,
        description: text("description"),
        version: plugin.version(),
        data_path: plugin.context.data.clone().unwrap_or_default(),
        skill_roots: plugin.skill_dirs.len(),
        skills: count_skills(&plugin.skill_dirs).await,
        command_roots: plugin.command_roots.len(),
        declared_mcp: declared_mcp(plugin),
        mcp: plugin
            .mcp_servers
            .iter()
            .map(|server| server.name.clone())
            .collect(),
        hooks: hook_details(plugin),
        user_config: plugin.manifest.get("userConfig").cloned(),
    }
}

/// 没加载的候选：只有 id、目录与 manifest 里的几项。
async fn skipped(
    id: &str,
    dir: &str,
    origin: &'static str,
    enabled: bool,
    data_root: Option<&str>,
) -> Listed {
    let manifest_file = manifest_path(dir).await;
    let manifest: Value = rust_cli_fs::read_optional(Path::new(&manifest_file))
        .await
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let (name, marketplace) = id.rsplit_once('@').unwrap_or((id, ""));
    let text = |key: &str| manifest.get(key).and_then(Value::as_str).map(str::to_owned);
    Listed {
        id: id.to_owned(),
        name: name.to_owned(),
        marketplace: marketplace.to_owned(),
        source: origin,
        root: dir.to_owned(),
        manifest_path: manifest_file,
        enabled,
        description: text("description"),
        version: text("version"),
        data_path: data_root
            .map(|root| {
                Path::new(root)
                    .join(rust_cli_base::plugin::sanitize_id(id))
                    .display()
                    .to_string()
            })
            .unwrap_or_default(),
        skill_roots: 0,
        skills: 0,
        command_roots: 0,
        declared_mcp: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        user_config: manifest.get("userConfig").cloned(),
    }
}

impl PluginAdmin {
    /// 列出（§13.2）。
    pub(crate) async fn list(&self, cwd: &str) -> Listing {
        let context = self.context(cwd).await;
        self.listing(&context).await
    }

    pub(super) async fn listing(&self, context: &Context) -> Listing {
        let plugins = &context.config.plugins;
        let side = EnvLayout::Controller.plugin_side(&plugins.root);
        let data_root = side.data_root.as_ref().map(|root| root.as_str().to_owned());
        let sources = sources(plugins, &context.cwd, side);
        // 前端撑不起来的插件是服务建的时候给的（`zcode plugin` 是不是 App 的那一份，与 TUI、`-p` 的加载相同；App 按 Host 声明的）
        let set = load(
            self.env.as_ref(),
            &context.workspace,
            &[],
            (&sources, &self.unhosted),
        )
        .await;
        let origin_of = |dir: &str| {
            sources
                .candidates
                .iter()
                .find(|candidate| candidate.dir.as_str() == dir)
                .map_or("inline", |candidate| origin_source(candidate.origin))
        };
        let mut listing = Listing::default();
        for plugin in set.plugins() {
            listing.plugins.push(listed(plugin).await);
        }
        for skip in set.skipped() {
            let Some(id) = &skip.id else {
                continue;
            };
            let (enabled, note) = match &skip.reason {
                SkipReason::Disabled => (false, None),
                SkipReason::NodeRuntime(_) | SkipReason::FrontendLacks(_) => (
                    true,
                    Some(format!(
                        "Plugin is enabled but not loaded in this runtime: {}",
                        skip.reason.describe()
                    )),
                ),
                _ => continue,
            };
            let mut entry = skipped(
                id,
                &skip.dir,
                origin_of(&skip.dir),
                enabled,
                data_root.as_deref(),
            )
            .await;
            if let Some(note) = note {
                listing
                    .diagnostics
                    .push(Diagnostic::warning(code::UNSUPPORTED_COMPONENT, note).of(id.clone()));
            }
            entry.enabled = enabled;
            listing.plugins.push(entry);
        }
        for error in set.errors() {
            let kind = match error.kind {
                PluginErrorKind::PathNotFound => code::ROOT_NOT_FOUND,
                PluginErrorKind::ManifestValidation => code::MANIFEST_INVALID,
                PluginErrorKind::McpConfigInvalid => code::MCP_INVALID,
                PluginErrorKind::Generic => code::COMPONENT_PATH_INVALID,
            };
            listing.diagnostics.push(
                Diagnostic::error(kind, error.message.clone())
                    .of(error.plugin.clone())
                    .at(error.path.clone().unwrap_or_default()),
            );
        }
        listing
    }

    /// 内置插件：id → 插件根与 manifest（内置目录或 Node 缓存里的 manifest 名字，§8.1）。
    pub(super) async fn builtins(context: &Context) -> BTreeMap<String, Builtin> {
        let mut found = BTreeMap::new();
        for root in &context.config.plugins.intents.official {
            for file in MANIFEST_PATHS.iter().map(|path| root.join(path)) {
                let Ok(Some(bytes)) = rust_cli_fs::read_optional(&file).await else {
                    continue;
                };
                let manifest: Value = serde_json::from_slice(&bytes).unwrap_or_default();
                if let Ok(name) = manifest::name(&manifest) {
                    found
                        .entry(format!("{name}@{OFFICIAL_MARKETPLACE}"))
                        .or_insert_with(|| Builtin {
                            root: root.clone(),
                            manifest,
                        });
                }
                break;
            }
        }
        found
    }
}
