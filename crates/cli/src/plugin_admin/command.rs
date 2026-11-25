//! `zcode plugin …`（plugin-store.md §13；别名 `zcode plugins`）：命令面照 Node 的 `zcode plugins`
//! （`apps/zcode-cli/packages/cli/src/plugins-command.ts`）。只调服务 [`PluginAdmin`]。

use std::collections::BTreeSet;
use std::path::Path;

use rust_cli_plugin_store::core::diagnostic::{Diagnostic, StoreError};
use rust_cli_plugin_store::core::id::PluginId;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::output::{
    available_json, available_list, diagnostic_lines, diagnostics_json, installed_json, json_text,
    marketplace_json, marketplace_list, plugin_json, plugin_list, warnings,
};
use super::{Control, InstallOutcome, PluginAdmin, Scope};

/// 主名与别名。
const NAMES: [&str; 2] = ["plugin", "plugins"];

pub(crate) const USAGE: &str = "Usage: zcode plugin <command> [options]

Commands:
  list [--json] [--available]                  List installed plugins; --available also lists the marketplace catalog
  install <plugin>[@marketplace] [-s <scope>]  Install a plugin from a known marketplace
  uninstall <plugin> [-s <scope>] [--keep-data] [--force]
                                               Uninstall a plugin (--keep-data keeps its data directory)
  enable <plugin> [-s <scope>]                 Enable a plugin
  disable [plugin] [-a|--all] [-s <scope>]     Disable a plugin, or every enabled plugin with --all
  update <plugin> [-s <scope>]                 Update a plugin to the latest marketplace version
  validate <path>                              Validate a plugin or marketplace manifest
  marketplace add <source> [--scope <scope>] [--sparse <path>]
                                               Add a marketplace from a URL, path, or GitHub repo
  marketplace list [--json]                    List configured marketplaces
  marketplace remove <name>                    Remove a configured marketplace
  marketplace update [name]                    Refresh one marketplace, or all when omitted

Scopes: user (default), project. `zcode plugins` is an alias of `zcode plugin`.";

/// 解析出的旗标。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    pub(crate) json: bool,
    pub(crate) verbose: bool,
    pub(crate) force: bool,
    pub(crate) available: bool,
    pub(crate) all: bool,
    pub(crate) keep_data: bool,
    pub(crate) scope: Option<String>,
    pub(crate) sparse: Vec<String>,
}

/// 解析出的命令：位置参数与旗标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginCommand {
    pub(crate) args: Vec<String>,
    pub(crate) flags: Flags,
}

/// 命令的输出：退出码、stdout、stderr。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) code: u8,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl Output {
    fn ok(stdout: String) -> Self {
        Self {
            code: 0,
            stdout,
            stderr: String::new(),
        }
    }

    fn fail(stderr: String) -> Self {
        Self {
            code: 1,
            stdout: String::new(),
            stderr,
        }
    }

    /// 用法错误：`<原因>\n<用法>`，退出码 1。
    fn usage(reason: &str) -> Self {
        let reason = if reason.is_empty() {
            String::new()
        } else {
            format!("{reason}\n")
        };
        Self::fail(format!("{reason}{USAGE}\n"))
    }

    /// 运行错误：`Error: <文字>`，退出码 1（Node `reportPluginsError`）。
    fn error(message: &str) -> Self {
        Self::fail(format!("Error: {message}\n"))
    }
}

/// `zcode plugin …`：不是这个命令时为 None；用法错误是完整的 stderr 文字。
pub(crate) fn parse(args: &[String]) -> Option<Result<PluginCommand, String>> {
    if !NAMES.contains(&args.first()?.as_str()) {
        return None;
    }
    let mut flags = Flags::default();
    let mut positionals = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |label: &str| {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or_else(|| format!("Option '{label} <value>' argument missing"))
        };
        let switch = |flag: &mut bool| {
            *flag = true;
            Ok(())
        };
        let parsed = match name {
            "--json" => switch(&mut flags.json),
            "--verbose" | "-v" => switch(&mut flags.verbose),
            "--force" | "-f" => switch(&mut flags.force),
            "--available" => switch(&mut flags.available),
            "--all" | "-a" => switch(&mut flags.all),
            "--keep-data" => switch(&mut flags.keep_data),
            "--scope" | "-s" => value("--scope").map(|v| flags.scope = Some(v)),
            "--sparse" => value("--sparse").map(|v| flags.sparse.push(v)),
            "--help" | "-h" => return Some(Err(format!("{USAGE}\n"))),
            other if other.starts_with('-') && other.len() > 1 => {
                Err(format!("Unknown option '{other}'"))
            }
            other => {
                positionals.push(other.to_owned());
                Ok(())
            }
        };
        if let Err(message) = parsed {
            return Some(Err(format!("{message}\n{USAGE}\n")));
        }
    }
    Some(Ok(PluginCommand {
        args: positionals,
        flags,
    }))
}

/// Node `resolveScope`：`user`、`project`（内部 workspace）；`local` 与别的报用法错误。
fn scope(value: Option<&str>) -> Result<Option<Scope>, Output> {
    match value {
        None => Ok(None),
        Some("user") => Ok(Some(Scope::User)),
        Some("project") => Ok(Some(Scope::Workspace)),
        Some("local") => Err(Output::usage(
            "Scope 'local' is not supported by zcode. Use: user, project",
        )),
        Some(other) => Err(Output::usage(&format!(
            "Invalid scope '{other}'. Use: user, project"
        ))),
    }
}

/// Node `requireOne`：正好一个非空的位置参数。
fn one(rest: &[String]) -> Result<String, Output> {
    match rest {
        [value] if !value.trim().is_empty() => Ok(value.trim().to_owned()),
        _ => Err(Output::usage("")),
    }
}

fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}

/// 卸载前的交互确认：收提示文字，回答是不是 `y` / `yes`。
pub(crate) type Confirm =
    Box<dyn FnMut(String) -> futures_util::future::BoxFuture<'static, bool> + Send>;

/// 运行需要的：服务、cwd、stdin 是不是终端、交互确认（卸载）、取消（Ctrl+C）。
pub(crate) struct Context<'a> {
    pub(crate) admin: &'a PluginAdmin,
    pub(crate) cwd: &'a str,
    pub(crate) interactive: bool,
    pub(crate) confirm: Confirm,
    pub(crate) cancel: CancellationToken,
}

pub(crate) async fn run(command: PluginCommand, context: &mut Context<'_>) -> Output {
    let PluginCommand { args, flags } = command;
    let (subcommand, rest) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        None => ("list", &[][..]),
    };
    let result = match subcommand {
        "list" if rest.is_empty() => Ok(list(context, &flags).await),
        "list" => Err(Output::usage("")),
        "install" => install(context, &flags, rest).await,
        "uninstall" => uninstall(context, &flags, rest).await,
        "enable" => set(context, &flags, rest, true).await,
        "disable" => disable(context, &flags, rest).await,
        "update" => update(context, &flags, rest).await,
        "validate" => validate(context, &flags, rest).await,
        "marketplace" => marketplace(context, &flags, rest).await,
        other => Err(Output::usage(&format!("Unknown plugins command: {other}"))),
    };
    result.unwrap_or_else(|output| output)
}

fn control(context: &Context<'_>) -> Control {
    Control::new(context.cancel.clone())
}

/// 装了、卸了、更新了之后清孤儿目录（§6.3）：子命令的进程里没有会话，正在用的集合为空。失败只记 warn。
async fn sweep(context: &Context<'_>) {
    if let Err(error) = context
        .admin
        .sweep_orphans(context.cwd, &BTreeSet::new(), &control(context))
        .await
    {
        tracing::warn!(%error, "orphaned plugin directories could not be swept");
    }
}

async fn list(context: &Context<'_>, flags: &Flags) -> Output {
    let listing = context.admin.list(context.cwd).await;
    let installed: Vec<Value> = listing
        .plugins
        .iter()
        .map(|plugin| plugin_json(plugin, &listing.diagnostics))
        .collect();
    if !flags.available {
        let stdout = if flags.json {
            json_text(&Value::Array(installed))
        } else {
            plugin_list(&listing, flags.verbose)
        };
        // 归不到条目的 error 级诊断不能静默丢掉（Node 相同）。
        let known: BTreeSet<&str> = listing.plugins.iter().map(|p| p.id.as_str()).collect();
        let orphaned: Vec<&Diagnostic> = listing
            .diagnostics
            .iter()
            .filter(|d| {
                d.is_error() && !d.plugin_id.as_deref().is_some_and(|id| known.contains(id))
            })
            .collect();
        if orphaned.is_empty() {
            return Output::ok(stdout);
        }
        return Output {
            code: 1,
            stdout,
            stderr: diagnostic_lines(orphaned),
        };
    }
    let catalog = context.admin.catalog(context.cwd, &control(context)).await;
    if flags.json {
        let mut diagnostics = listing.diagnostics.clone();
        diagnostics.extend(catalog.diagnostics.iter().cloned());
        return Output::ok(json_text(&json!({
            "installed": installed,
            "available": catalog.available.iter().map(available_json).collect::<Vec<_>>(),
            "diagnostics": diagnostics_json(&diagnostics),
        })));
    }
    Output::ok(format!(
        "{}\n{}",
        plugin_list(&listing, flags.verbose),
        available_list(&catalog.available)
    ))
}

async fn install(context: &Context<'_>, flags: &Flags, rest: &[String]) -> Result<Output, Output> {
    let identifier = one(rest)?;
    scope(flags.scope.as_deref())?;
    let (name, marketplace) = PluginId::split_identifier(&identifier);
    let marketplace = match marketplace {
        Some(marketplace) => marketplace,
        None => {
            let catalog = context.admin.catalog(context.cwd, &control(context)).await;
            let matches: Vec<_> = catalog
                .available
                .iter()
                .filter(|plugin| plugin.name == name)
                .collect();
            match matches.as_slice() {
                [one] => one.marketplace.clone(),
                [] => {
                    return Err(Output::error(&format!(
                        "Plugin not found in any marketplace: {name}"
                    )));
                }
                many => {
                    let ids: Vec<&str> = many.iter().map(|plugin| plugin.id.as_str()).collect();
                    return Err(Output::error(&format!(
                        "Plugin name is ambiguous, use <plugin>@<marketplace>: {}",
                        ids.join(", ")
                    )));
                }
            }
        }
    };
    let id = PluginId::new(&name, &marketplace);
    let outcome = context
        .admin
        .install(context.cwd, &id, &control(context))
        .await;
    if !has_errors(&outcome.diagnostics) {
        sweep(context).await;
    }
    Ok(report_install(&outcome, &id.to_string(), flags.json))
}

/// Node `reportInstallOutcome`。
fn report_install(outcome: &InstallOutcome, plugin_id: &str, json: bool) -> Output {
    let failed = has_errors(&outcome.diagnostics);
    if json {
        let stdout = json_text(&json!({
            "pluginId": plugin_id,
            "ok": !failed,
            "installed": outcome.installed.iter().map(installed_json).collect::<Vec<_>>(),
            "dependencyClosure": outcome.closure,
            "diagnostics": diagnostics_json(&outcome.diagnostics),
        }));
        return Output {
            code: u8::from(failed),
            stdout,
            stderr: String::new(),
        };
    }
    if failed {
        return Output::fail(format!(
            "Failed to install plugin {plugin_id}\n{}",
            diagnostic_lines(&outcome.diagnostics)
        ));
    }
    let stdout = outcome
        .installed
        .iter()
        .map(|plugin| {
            let version = if plugin.version.is_empty() {
                String::new()
            } else {
                format!(" ({})", plugin.version)
            };
            let state = if plugin.enabled {
                "enabled"
            } else {
                "disabled"
            };
            format!("Installed plugin {}{version} [{state}]\n", plugin.id)
        })
        .collect();
    Output {
        code: 0,
        stdout,
        stderr: warnings(&outcome.diagnostics),
    }
}

/// Node `resolveLoadedPluginId`：带 `@` 的原样；裸名字在列出的插件里唯一才行。
async fn loaded_id(context: &Context<'_>, identifier: &str) -> Result<String, Output> {
    if PluginId::parse(identifier).is_some() {
        return Ok(identifier.to_owned());
    }
    let listing = context.admin.list(context.cwd).await;
    let matches: Vec<&str> = listing
        .plugins
        .iter()
        .filter(|plugin| plugin.name == identifier)
        .map(|plugin| plugin.id.as_str())
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(Output::error(&format!("Plugin not found: {identifier}"))),
        many => Err(Output::error(&format!(
            "Plugin name is ambiguous, use <plugin>@<marketplace>: {}",
            many.join(", ")
        ))),
    }
}

async fn update(context: &Context<'_>, flags: &Flags, rest: &[String]) -> Result<Output, Output> {
    let identifier = one(rest)?;
    scope(flags.scope.as_deref())?;
    let id = loaded_id(context, &identifier).await?;
    let (outcome, previous) = context
        .admin
        .update(context.cwd, &id, &control(context))
        .await
        .map_err(|e| Output::error(&e.to_string()))?;
    let failed = has_errors(&outcome.diagnostics);
    if !failed {
        sweep(context).await;
    }
    let current = outcome
        .installed
        .iter()
        .find(|plugin| plugin.id == id)
        .map(|plugin| plugin.version.clone());
    if flags.json {
        let mut object = serde_json::Map::new();
        object.insert("pluginId".into(), id.clone().into());
        object.insert("ok".into(), (!failed).into());
        object.insert("previousVersion".into(), previous.clone().into());
        if let Some(version) = current.filter(|v| !v.is_empty()) {
            object.insert("version".into(), version.into());
        }
        object.insert(
            "installed".into(),
            outcome
                .installed
                .iter()
                .map(installed_json)
                .collect::<Vec<_>>()
                .into(),
        );
        object.insert("diagnostics".into(), diagnostics_json(&outcome.diagnostics));
        return Ok(Output {
            code: u8::from(failed),
            stdout: json_text(&Value::Object(object)),
            stderr: String::new(),
        });
    }
    if failed {
        return Ok(Output::fail(format!(
            "Failed to update plugin {id}\n{}",
            diagnostic_lines(&outcome.diagnostics)
        )));
    }
    let version = current.unwrap_or_default();
    let stdout = if !version.is_empty() && version == previous {
        format!("Plugin {id} is already up to date ({version}).\n")
    } else {
        let or_unknown = |v: &str| {
            if v.is_empty() {
                "unknown".to_owned()
            } else {
                v.to_owned()
            }
        };
        format!(
            "Updated plugin {id} from {} to {}. Restart zcode to apply.\n",
            or_unknown(&previous),
            or_unknown(&version)
        )
    };
    Ok(Output {
        code: 0,
        stdout,
        stderr: warnings(&outcome.diagnostics),
    })
}

async fn uninstall(
    context: &mut Context<'_>,
    flags: &Flags,
    rest: &[String],
) -> Result<Output, Output> {
    let identifier = one(rest)?;
    scope(flags.scope.as_deref())?;
    let target = loaded_id(context, &identifier).await?;
    // 卸载是彻底的清除：交互终端里问；非交互又没带 --force 时拒绝（Node 的安全阀）。
    if !flags.force {
        if !context.interactive {
            return Ok(Output::fail(format!(
                "Refusing to uninstall {target} without confirmation. Re-run with --force in a non-interactive shell.\n"
            )));
        }
        if !(context.confirm)(format!("Uninstall {target}? [y/N] ")).await {
            return Ok(Output::ok(format!(
                "Aborted. {target} was not uninstalled.\n"
            )));
        }
    }
    let removed = context
        .admin
        .uninstall(context.cwd, &target, flags.keep_data, &control(context))
        .await
        .map_err(|error| Output::error(&error.to_string()))?;
    if removed.is_some() {
        sweep(context).await;
    }
    let stdout = if flags.json {
        let mut object = serde_json::Map::new();
        object.insert(
            "pluginId".into(),
            removed
                .as_ref()
                .map_or(target.clone(), |r| r.view.id.clone())
                .into(),
        );
        object.insert("removed".into(), removed.is_some().into());
        if flags.keep_data {
            object.insert("keptData".into(), true.into());
        }
        if let Some(removed) = &removed {
            object.insert("plugin".into(), installed_json(&removed.view));
        }
        json_text(&Value::Object(object))
    } else {
        match &removed {
            Some(removed) => format!("Uninstalled plugin {}\n", removed.view.id),
            None => format!("Plugin not installed: {target}\n"),
        }
    };
    Ok(Output {
        code: u8::from(removed.is_none()),
        stdout,
        stderr: String::new(),
    })
}

async fn set(
    context: &Context<'_>,
    flags: &Flags,
    rest: &[String],
    enabled: bool,
) -> Result<Output, Output> {
    let plugin = one(rest)?;
    let scope = scope(flags.scope.as_deref())?.unwrap_or(Scope::User);
    let result = context
        .admin
        .set_enabled(
            context.cwd,
            &plugin,
            scope,
            Some(enabled),
            &control(context),
        )
        .await
        .map_err(|error| Output::error(&error.to_string()))?;
    Ok(Output::ok(if flags.json {
        json_text(
            &json!({"enabled": enabled, "path": result.path, "plugin": plugin_json(&result.plugin, &[])}),
        )
    } else {
        format!(
            "{} plugin {} in {}\n",
            if enabled { "Enabled" } else { "Disabled" },
            result.plugin.id,
            result.path
        )
    }))
}

async fn disable(context: &Context<'_>, flags: &Flags, rest: &[String]) -> Result<Output, Output> {
    if flags.all && !rest.is_empty() {
        return Err(Output::usage("Cannot use --all with a specific plugin"));
    }
    if !flags.all {
        if rest.is_empty() {
            return Err(Output::usage(
                "Please specify a plugin name or use --all to disable all plugins",
            ));
        }
        return set(context, flags, rest, false).await;
    }
    if flags.scope.is_some() {
        return Err(Output::usage("Cannot use --scope with --all"));
    }
    let listing = context.admin.list(context.cwd).await;
    let mut entries = Vec::new();
    for plugin in listing.plugins.iter().filter(|plugin| plugin.enabled) {
        let result = context
            .admin
            .set_enabled(
                context.cwd,
                &plugin.id,
                Scope::User,
                Some(false),
                &control(context),
            )
            .await;
        entries.push((plugin.clone(), result));
    }
    // 写的是用户级；项目层的启用优先级更高、盖不掉，所以按生效值复查（Node 相同）。
    let still: BTreeSet<String> = context
        .admin
        .list(context.cwd)
        .await
        .plugins
        .into_iter()
        .filter(|p| p.enabled)
        .map(|p| p.id)
        .collect();
    let failed = |plugin: &super::Listed, result: &Result<super::SetEnabled, StoreError>| {
        result.is_err() || still.contains(&plugin.id)
    };
    let any_failed = entries
        .iter()
        .any(|(plugin, result)| failed(plugin, result));
    if flags.json {
        let items: Vec<Value> = entries
            .iter()
            .map(|(plugin, result)| {
                let mut object = serde_json::Map::new();
                let shown = result.as_ref().map_or(plugin, |r| &r.plugin);
                object.insert(
                    "enabled".into(),
                    result.as_ref().map_or(plugin.enabled, |_| false).into(),
                );
                object.insert("effectiveEnabled".into(), still.contains(&plugin.id).into());
                object.insert(
                    "path".into(),
                    result
                        .as_ref()
                        .map_or(Value::Null, |r| r.path.clone().into()),
                );
                object.insert("plugin".into(), plugin_json(shown, &[]));
                if let Err(error) = result {
                    object.insert("error".into(), error.to_string().into());
                }
                Value::Object(object)
            })
            .collect();
        return Ok(Output {
            code: u8::from(any_failed),
            stdout: json_text(&Value::Array(items)),
            stderr: String::new(),
        });
    }
    if entries.is_empty() {
        return Ok(Output::ok("No enabled plugins.\n".to_owned()));
    }
    let mut output = Output {
        code: u8::from(any_failed),
        ..Output::default()
    };
    for (plugin, result) in &entries {
        if let Ok(result) = result
            && !failed(plugin, &Ok(result.clone()))
        {
            output.stdout.push_str(&format!(
                "Disabled plugin {} in {}\n",
                result.plugin.id, result.path
            ));
        }
    }
    for (plugin, result) in entries
        .iter()
        .filter(|(plugin, result)| failed(plugin, result))
    {
        output.stderr.push_str(&match result {
            Err(error) => format!("Failed to disable plugin {}: {error}\n", plugin.id),
            Ok(_) => format!(
                "Warning: plugin {} is still enabled by a higher-priority config layer (project/workspace); disable it there with --scope project.\n",
                plugin.id
            ),
        });
    }
    Ok(output)
}

async fn validate(context: &Context<'_>, flags: &Flags, rest: &[String]) -> Result<Output, Output> {
    let input = one(rest)?;
    let path = rust_cli_base::path::normalize(&Path::new(context.cwd).join(&input));
    let diagnostics = rust_cli_plugin_store::local::validate::validate_path(&path).await;
    let failed = has_errors(&diagnostics);
    if flags.json {
        return Ok(Output {
            code: u8::from(failed),
            stdout: json_text(
                &json!({"path": input, "ok": !failed, "diagnostics": diagnostics_json(&diagnostics)}),
            ),
            stderr: String::new(),
        });
    }
    if failed {
        return Ok(Output::fail(format!(
            "Plugin manifest is invalid: {input}\n{}",
            diagnostic_lines(&diagnostics)
        )));
    }
    Ok(Output {
        code: 0,
        stdout: format!("Plugin manifest is valid: {input}\n"),
        stderr: warnings(&diagnostics),
    })
}

async fn marketplace(
    context: &Context<'_>,
    flags: &Flags,
    args: &[String],
) -> Result<Output, Output> {
    let Some((action, rest)) = args.split_first() else {
        return Err(Output::usage("Missing marketplace command"));
    };
    let admin = context.admin;
    match action.as_str() {
        "add" => {
            // 市场只有用户级一份：--scope 只校验拼写。
            scope(flags.scope.as_deref())?;
            let source = one(rest)?;
            let record = admin
                .marketplace_add(context.cwd, &source, &flags.sparse, &control(context))
                .await
                .map_err(|error| Output::error(&error.to_string()))?;
            Ok(Output::ok(if flags.json {
                json_text(&marketplace_json(&record))
            } else {
                format!(
                    "Added marketplace {} ({} plugins)\n",
                    record.id, record.plugin_count
                )
            }))
        }
        "list" => {
            if !rest.is_empty() {
                return Err(Output::usage(""));
            }
            let catalog = admin.catalog(context.cwd, &control(context)).await;
            Ok(Output::ok(if flags.json {
                json_text(&Value::Array(
                    catalog.marketplaces.iter().map(marketplace_json).collect(),
                ))
            } else {
                marketplace_list(&catalog.marketplaces)
            }))
        }
        "remove" => {
            let name = one(rest)?;
            admin
                .marketplace_remove(context.cwd, &name, &control(context))
                .await
                .map_err(|error| Output::error(&error.to_string()))?;
            Ok(Output::ok(if flags.json {
                json_text(&json!({"marketplace": name, "removed": true}))
            } else {
                format!("Removed marketplace {name}\n")
            }))
        }
        "update" => {
            if rest.len() > 1 {
                return Err(Output::usage(""));
            }
            let name = rest
                .first()
                .map(|name| name.trim())
                .filter(|name| !name.is_empty());
            let refreshed = admin
                .marketplace_refresh(context.cwd, name, &control(context))
                .await
                .map_err(|error| Output::error(&error.to_string()))?;
            let failed = has_errors(&refreshed.diagnostics);
            if flags.json {
                return Ok(Output {
                    code: u8::from(failed),
                    stdout: json_text(&json!({
                        "ok": !failed,
                        "marketplaces": refreshed.marketplaces.iter().map(marketplace_json).collect::<Vec<_>>(),
                        "diagnostics": diagnostics_json(&refreshed.diagnostics),
                    })),
                    stderr: String::new(),
                });
            }
            let mut output = Output::default();
            for record in &refreshed.marketplaces {
                output.stdout.push_str(&format!(
                    "Updated marketplace {} ({} plugins)\n",
                    record.id, record.plugin_count
                ));
            }
            if failed {
                output.code = 1;
                output.stderr = format!(
                    "Marketplace update failed\n{}",
                    diagnostic_lines(&refreshed.diagnostics)
                );
            } else {
                output.stderr = warnings(&refreshed.diagnostics);
            }
            Ok(output)
        }
        other => Err(Output::usage(&format!(
            "Unknown marketplace command: {other}"
        ))),
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
