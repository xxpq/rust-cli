//! `zcode plugin validate <path>`（plugin-store.md §13.4，Node `validateLocalPluginPath`）：只读，不拿锁，不联网。路径是插件
//! 目录、市场目录或其中的 manifest 文件；根下有市场清单就当市场校验，否则当插件根校验。远端来源的条目只报「推迟」的警告。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::catalog::{self, Catalog};
use crate::core::closure::closure;
use crate::core::diagnostic::{Diagnostic, code};
use crate::core::id::{INLINE_MARKETPLACE, PluginId};
use crate::core::manifest::{self, CATALOG_PATHS, MANIFEST_PATHS};
use crate::core::validate::manifest_warnings;

/// 远端来源的种类：校验推迟到安装（Node `getMarketplaceSourceValidationDeferral`）。
const REMOTE_KINDS: [&str; 4] = ["github", "git", "url", "git-subdir"];

/// manifest 文件所在的插件根（`.xxx-plugin/plugin.json` 的上一层，Node `resolveManifestRootFromFile`）。
fn root_of_file(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(file).to_path_buf();
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.starts_with('.') && name.ends_with("-plugin") {
        return dir.parent().map_or(dir.clone(), Path::to_path_buf);
    }
    dir
}

async fn first_file(root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    for candidate in candidates {
        let path = super::zip::within(root, candidate);
        if rust_cli_fs::is_file(&path).await {
            return Some(path);
        }
    }
    None
}

async fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = rust_cli_fs::read_optional(path)
        .await
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// 校验 `path`（已是绝对路径）。
pub async fn validate_path(path: &Path) -> Vec<Diagnostic> {
    let shown = path.display().to_string();
    let root = if rust_cli_fs::is_dir(path).await {
        path.to_path_buf()
    } else if rust_cli_fs::is_file(path).await {
        root_of_file(path)
    } else {
        return vec![
            Diagnostic::error(
                code::MANIFEST_NOT_FOUND,
                format!("Path does not exist: {shown}"),
            )
            .at(shown),
        ];
    };
    if let Some(file) = first_file(&root, &CATALOG_PATHS).await {
        return validate_marketplace(&root, &file).await;
    }
    validate_plugin_root(&root, None, INLINE_MARKETPLACE).await
}

/// 一个插件根：manifest 在、是对象、名字合规（`expected` 给了时要等于它），再加兼容性警告。
async fn validate_plugin_root(
    root: &Path,
    expected: Option<&str>,
    marketplace: &str,
) -> Vec<Diagnostic> {
    let shown = root.display().to_string();
    let Some(file) = first_file(root, &MANIFEST_PATHS).await else {
        let id = expected.map(|name| PluginId::new(name, marketplace).to_string());
        let message = match &id {
            Some(id) => format!("Plugin manifest not found: {id}"),
            None => format!("Plugin manifest not found: {shown}"),
        };
        let diagnostic = Diagnostic::error(code::MANIFEST_NOT_FOUND, message).at(shown);
        return vec![match id {
            Some(id) => diagnostic.of(id),
            None => diagnostic,
        }];
    };
    let file_shown = file.display().to_string();
    let value = match read_json(&file).await {
        Ok(value) => value,
        Err(error) => return vec![Diagnostic::error(code::MANIFEST_INVALID, error).at(file_shown)],
    };
    let name = match manifest::name(&value) {
        Ok(name) => name,
        Err(error) => return vec![Diagnostic::error(code::MANIFEST_INVALID, error).at(file_shown)],
    };
    let id = PluginId::new(expected.unwrap_or(&name), marketplace).to_string();
    let mut found = Vec::new();
    if let Some(expected) = expected
        && expected != name
    {
        found.push(
            Diagnostic::error(
                code::MANIFEST_INVALID,
                format!(
                    "Plugin manifest name '{name}' does not match marketplace entry '{expected}'"
                ),
            )
            .at(&file_shown)
            .of(&id),
        );
    }
    found.extend(manifest_warnings(&value, &file_shown, &id));
    found
}

/// 一个市场：清单合规；没有插件时警告；每个条目：有来源、依赖闭包成立、本地来源的插件根合规，远端来源推迟。
async fn validate_marketplace(root: &Path, file: &Path) -> Vec<Diagnostic> {
    let file_shown = file.display().to_string();
    let catalog: Catalog = match read_json(file).await.ok().as_ref().and_then(catalog::parse) {
        Some(catalog) => catalog,
        None => {
            return vec![
                Diagnostic::error(code::MARKETPLACE_INVALID, "Marketplace manifest is invalid")
                    .at(file_shown),
            ];
        }
    };
    let mut found = Vec::new();
    if catalog.plugins.is_empty() {
        found.push(Diagnostic::warning(
            code::MARKETPLACE_INVALID,
            format!("Marketplace has no plugins: {}", catalog.name),
        ));
    }
    let base = match &catalog.plugin_root {
        Some(plugin_root) => root.join(plugin_root.trim_start_matches("./")),
        None => root.to_path_buf(),
    };
    for entry in &catalog.plugins {
        let id = PluginId::new(&entry.name, &catalog.name);
        let label = id.to_string();
        if let Err(error) = closure(&id, |name| (name == catalog.name).then_some(&catalog)) {
            found.push(error.to_diagnostic(Some(&label)));
        }
        let kind = entry
            .source
            .as_ref()
            .and_then(|s| s.get("source"))
            .and_then(Value::as_str);
        let relative = match &entry.source {
            None => {
                found.push(
                    Diagnostic::error(
                        code::MARKETPLACE_INVALID,
                        format!("Plugin has no install source: {label}"),
                    )
                    .of(&label),
                );
                continue;
            }
            Some(Value::String(path)) => Some(path.clone()),
            Some(_) if kind.is_some_and(|k| REMOTE_KINDS.contains(&k)) => {
                found.push(
                    Diagnostic::warning(
                        code::VALIDATION_DEFERRED,
                        format!("Remote plugin source validation is deferred until install or single-plugin validate: {label}"),
                    )
                    .of(&label),
                );
                continue;
            }
            Some(_) if matches!(kind, Some("npm" | "pip")) => {
                found.push(
                    Diagnostic::warning(
                        code::SOURCE_UNSUPPORTED,
                        format!(
                            "Plugin source is recognized but not supported in V1 install: {}",
                            kind.unwrap_or_default()
                        ),
                    )
                    .of(&label),
                );
                continue;
            }
            Some(source) => source
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        let Some(relative) = relative else {
            continue;
        };
        let dir = base.join(relative.trim_start_matches("./"));
        if !rust_cli_fs::is_dir(&dir).await {
            found.push(
                Diagnostic::error(
                    code::MARKETPLACE_INVALID,
                    format!("Plugin source directory does not exist: {relative}"),
                )
                .of(&label),
            );
            continue;
        }
        found.extend(validate_plugin_root(&dir, Some(&entry.name), &catalog.name).await);
    }
    found
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::fixture::Fixture;
    use super::*;

    #[tokio::test]
    async fn plugins_and_marketplaces_are_checked_without_writing() {
        let fixture = Fixture::new();
        fixture
            .json(
                "kit/.claude-plugin/plugin.json",
                &json!({"name": "kit", "outputStyles": "./s"}),
            )
            .await;
        let found = validate_path(&fixture.path("kit/.claude-plugin/plugin.json")).await;
        assert_eq!(
            found.iter().map(|d| d.code).collect::<Vec<_>>(),
            [code::UNSUPPORTED_COMPONENT]
        );
        assert!(!found[0].is_error());

        fixture
            .json("bad/.zcode-plugin/plugin.json", &json!({"name": "Bad"}))
            .await;
        assert!(validate_path(&fixture.path("bad")).await[0].is_error());
        assert_eq!(
            validate_path(&fixture.path("missing")).await[0].code,
            code::MANIFEST_NOT_FOUND
        );

        fixture
            .json(
                "mkt/kit/.claude-plugin/plugin.json",
                &json!({"name": "kit"}),
            )
            .await;
        fixture
            .json(
                "mkt/marketplace.json",
                &json!({"name": "mkt", "plugins": [
                    {"name": "kit", "source": "./kit"},
                    {"name": "far", "source": {"source": "github", "repo": "a/b"}},
                    {"name": "none"},
                    {"name": "gone", "source": "./gone"}
                ]}),
            )
            .await;
        let found = validate_path(&fixture.path("mkt")).await;
        let codes: Vec<(&str, bool)> = found.iter().map(|d| (d.code, d.is_error())).collect();
        assert_eq!(
            codes,
            [
                (code::VALIDATION_DEFERRED, false),
                (code::MARKETPLACE_INVALID, true),
                (code::MARKETPLACE_INVALID, true)
            ]
        );
        assert!(!rust_cli_fs::is_dir(&fixture.path("home")).await);
    }
}
