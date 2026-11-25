//! `zcode plugin` 的输出（plugin-store.md §13.3）：文字与 JSON 照 Node（`plugins-command-format.ts`）。JSON 两格缩进加换行，键的
//! 顺序同 Node 的对象字面量。

use rust_cli_plugin_store::core::diagnostic::Diagnostic;
use rust_cli_plugin_store::core::records::Known;
use serde_json::{Map, Value, json};

use super::listing::{HookDetail, Listed, Listing};
use super::writes::{Available, InstalledView};

/// `formatJson`：`JSON.stringify(value, null, 2)` 加换行。
pub(crate) fn json_text(value: &Value) -> String {
    rust_cli_plugin_store::core::json_text(value)
}

fn put(object: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value.into());
    }
}

/// 一条诊断的文字（Node `formatDiagnosticLine`）。
pub(crate) fn diagnostic_line(diagnostic: &Diagnostic) -> String {
    let location = diagnostic
        .path
        .as_ref()
        .filter(|p| !p.is_empty())
        .map(|p| format!(" ({p})"))
        .unwrap_or_default();
    let plugin = diagnostic
        .plugin_id
        .as_ref()
        .map(|id| format!(" {id}"))
        .unwrap_or_default();
    format!(
        "- [{}]{plugin} {}: {}{location}\n",
        diagnostic.severity.as_str(),
        diagnostic.code,
        diagnostic.message
    )
}

pub(crate) fn diagnostic_lines<'a>(
    diagnostics: impl IntoIterator<Item = &'a Diagnostic>,
) -> String {
    diagnostics.into_iter().map(diagnostic_line).collect()
}

/// warning 级的诊断（Node `writeWarnings`）。
pub(crate) fn warnings(diagnostics: &[Diagnostic]) -> String {
    diagnostic_lines(diagnostics.iter().filter(|d| !d.is_error()))
}

pub(crate) fn diagnostics_json(diagnostics: &[Diagnostic]) -> Value {
    Value::Array(diagnostics.iter().map(Diagnostic::to_json).collect())
}

pub(crate) fn hook_json(hook: &HookDetail, root: &str) -> Value {
    let text = |key: &str| hook.hook.get(key).cloned();
    let mut object = Map::new();
    object.insert(
        "command".into(),
        text("command").unwrap_or_else(|| Value::String(String::new())),
    );
    object.insert("event".into(), hook.event.clone().into());
    // rust 不判断 hook 能不能运行（plugin-store.md §15 第 4 条）。
    object.insert("runnable".into(), true.into());
    object.insert("sourcePath".into(), root.into());
    object.insert(
        "type".into(),
        text("type").unwrap_or_else(|| "command".into()),
    );
    for key in ["args", "async"] {
        put(&mut object, key, text(key));
    }
    put(&mut object, "matcher", hook.matcher.clone());
    for key in ["shell", "statusMessage", "timeout", "timeoutMs"] {
        put(&mut object, key, text(key));
    }
    Value::Object(object)
}

/// 列出的一个插件（Node `formatPluginJson`）；`diagnostics` 只放这个插件的。
pub(crate) fn plugin_json(plugin: &Listed, diagnostics: &[Diagnostic]) -> Value {
    let own: Vec<Value> = diagnostics
        .iter()
        .filter(|d| d.plugin_id.as_deref() == Some(plugin.id.as_str()))
        .map(Diagnostic::to_json)
        .collect();
    let mut object = Map::new();
    object.insert("commandRootCount".into(), plugin.command_roots.into());
    object.insert("dataPath".into(), plugin.data_path.clone().into());
    object.insert("enabled".into(), plugin.enabled.into());
    object.insert("id".into(), plugin.id.clone().into());
    object.insert("manifestPath".into(), plugin.manifest_path.clone().into());
    object.insert("marketplace".into(), plugin.marketplace.clone().into());
    object.insert("declaredMcpServerNames".into(), json!(plugin.declared_mcp));
    object.insert("mcpServerNames".into(), json!(plugin.mcp));
    object.insert(
        "hookDetails".into(),
        Value::Array(
            plugin
                .hooks
                .iter()
                .map(|h| hook_json(h, &plugin.root))
                .collect(),
        ),
    );
    object.insert("name".into(), plugin.name.clone().into());
    object.insert("rootPath".into(), plugin.root.clone().into());
    object.insert("skillCount".into(), plugin.skills.into());
    object.insert("skillRootCount".into(), plugin.skill_roots.into());
    object.insert("source".into(), plugin.source.into());
    object.insert("diagnostics".into(), Value::Array(own));
    put(&mut object, "description", plugin.description.clone());
    put(&mut object, "version", plugin.version.clone());
    Value::Object(object)
}

/// 文字的插件清单（Node `formatHumanPluginList`）。
pub(crate) fn plugin_list(listing: &Listing, verbose: bool) -> String {
    if listing.plugins.is_empty() {
        return "No plugins found.\n".to_owned();
    }
    let mut lines = vec![format!("Plugins ({})", listing.plugins.len())];
    for plugin in &listing.plugins {
        let mcp = if plugin.mcp.is_empty() {
            "none".to_owned()
        } else {
            plugin.mcp.join(", ")
        };
        lines.push(format!(
            "- {} [{}]",
            plugin.id,
            if plugin.enabled {
                "enabled"
            } else {
                "disabled"
            }
        ));
        lines.push(format!(
            "  {}/{}: {}",
            plugin.source, plugin.marketplace, plugin.root
        ));
        lines.push(format!(
            "  skills: {}, commands: {}, hooks: {}, mcp: {mcp}",
            plugin.skills,
            plugin.command_roots,
            plugin.hooks.len()
        ));
    }
    if verbose && !listing.diagnostics.is_empty() {
        lines.push(String::new());
        lines.push(format!("Diagnostics ({})", listing.diagnostics.len()));
        for diagnostic in &listing.diagnostics {
            lines.push(diagnostic_line(diagnostic).trim_end().to_owned());
        }
    }
    format!("{}\n", lines.join("\n"))
}

/// 可装的条目（Node `formatAvailablePluginJson`）。
pub(crate) fn available_json(plugin: &Available) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), plugin.id.clone().into());
    object.insert("installed".into(), plugin.installed.into());
    object.insert("marketplace".into(), plugin.marketplace.clone().into());
    object.insert("name".into(), plugin.name.clone().into());
    put(&mut object, "description", plugin.description.clone());
    put(&mut object, "version", plugin.version.clone());
    Value::Object(object)
}

/// 文字的可装清单（Node `formatHumanAvailableList`）。
pub(crate) fn available_list(plugins: &[Available]) -> String {
    if plugins.is_empty() {
        return "No plugins available from configured marketplaces.\n".to_owned();
    }
    let mut lines = vec![format!("Available plugins ({})", plugins.len())];
    for plugin in plugins {
        let version = plugin
            .version
            .as_ref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        let state = if plugin.installed {
            "installed"
        } else {
            "not installed"
        };
        lines.push(format!("- {}{version} [{state}]", plugin.id));
        if let Some(description) = &plugin.description {
            lines.push(format!("  {description}"));
        }
    }
    format!("{}\n", lines.join("\n"))
}

/// 装好的记录（Node `formatInstalledPluginJson`）。
pub(crate) fn installed_json(plugin: &InstalledView) -> Value {
    let mut object = Map::new();
    object.insert("enabled".into(), plugin.enabled.into());
    object.insert("id".into(), plugin.id.clone().into());
    object.insert("marketplace".into(), plugin.marketplace.clone().into());
    object.insert("name".into(), plugin.name.clone().into());
    object.insert("scope".into(), plugin.scope.clone().into());
    put(
        &mut object,
        "installPath",
        Some(plugin.install_path.clone()).filter(|p| !p.is_empty()),
    );
    put(&mut object, "installedAt", plugin.installed_at.clone());
    put(
        &mut object,
        "version",
        Some(plugin.version.clone()).filter(|v| !v.is_empty()),
    );
    Value::Object(object)
}

/// 市场摘要（Node `formatMarketplaceJson`）。
pub(crate) fn marketplace_json(record: &Known) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), record.id.clone().into());
    object.insert(
        "isOfficial".into(),
        super::PluginAdmin::is_official(&record.id).into(),
    );
    object.insert("name".into(), record.name.clone().into());
    object.insert("pluginCount".into(), record.plugin_count.into());
    object.insert("source".into(), record.source.raw.clone());
    put(&mut object, "description", record.description.clone());
    put(&mut object, "lastUpdated", record.last_updated.clone());
    put(
        &mut object,
        "refreshFailure",
        record.refresh_failure.clone(),
    );
    Value::Object(object)
}

/// 文字的市场清单（Node `formatHumanMarketplaceList`）。
pub(crate) fn marketplace_list(records: &[Known]) -> String {
    if records.is_empty() {
        return "No marketplaces configured.\n".to_owned();
    }
    let mut lines = vec![format!("Marketplaces ({})", records.len())];
    for record in records {
        let official = if super::PluginAdmin::is_official(&record.id) {
            " [official]"
        } else {
            ""
        };
        lines.push(format!(
            "- {}{official}: {} plugins",
            record.id, record.plugin_count
        ));
        let raw = &record.source.raw;
        let kind = raw
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let locator = ["url", "repo", "path", "package"]
            .iter()
            .find_map(|key| raw.get(*key).and_then(Value::as_str));
        lines.push(match locator {
            Some(locator) => format!("  source: {kind} {locator}"),
            None => format!("  source: {kind}"),
        });
        if let Some(message) = record
            .refresh_failure
            .as_ref()
            .and_then(|f| f.get("message"))
            .and_then(Value::as_str)
        {
            lines.push(format!("  last refresh failed: {message}"));
        }
    }
    format!("{}\n", lines.join("\n"))
}
