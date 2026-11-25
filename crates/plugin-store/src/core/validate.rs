//! `zcode plugin validate` 里不做 IO 的检查（plugin-store.md §13.4，Node `pushManifestCompatibilityDiagnostics`）。

use serde_json::Value;

use super::diagnostic::{Diagnostic, code};
use super::manifest::UNSUPPORTED_FIELDS;

/// manifest 的兼容性警告：只诊断的组件、必填又没有缺省的配置项、MCPB 包。
pub fn manifest_warnings(manifest: &Value, path: &str, plugin_id: &str) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    let Some(object) = manifest.as_object() else {
        return found;
    };
    for key in UNSUPPORTED_FIELDS {
        if object.contains_key(key) {
            found.push(
                Diagnostic::warning(
                    code::UNSUPPORTED_COMPONENT,
                    format!("Plugin component is diagnostic-only in this ZCode runtime: {key}"),
                )
                .at(path)
                .of(plugin_id),
            );
        }
    }
    for (key, option) in object
        .get("userConfig")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if option.get("required") == Some(&Value::Bool(true)) && option.get("default").is_none() {
            found.push(
                Diagnostic::warning(
                    code::VARIABLE_MISSING,
                    format!(
                        "Required plugin userConfig has no default and must be configured: {key}"
                    ),
                )
                .at(path)
                .of(plugin_id),
            );
        }
    }
    if object.get("mcpServers").is_some_and(contains_bundle) {
        found.push(
            Diagnostic::warning(
                code::SOURCE_UNSUPPORTED,
                "MCPB/DXT plugin bundles are recognized but not supported in this runtime",
            )
            .at(path)
            .of(plugin_id),
        );
    }
    found
}

/// Node `containsMcpBundleSource`：字符串以 `.mcpb` / `.dxt` 结尾，或数组里有这样的项。
fn contains_bundle(value: &Value) -> bool {
    match value {
        Value::String(text) => text.ends_with(".mcpb") || text.ends_with(".dxt"),
        Value::Array(items) => items.iter().any(contains_bundle),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn warnings_follow_node() {
        let found = manifest_warnings(
            &json!({
                "name": "kit",
                "outputStyles": "./styles",
                "userConfig": {"token": {"required": true}, "url": {"required": true, "default": "x"}},
                "mcpServers": ["./server.mcpb"]
            }),
            "/k/.claude-plugin/plugin.json",
            "kit@inline",
        );
        let codes: Vec<&str> = found.iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            [
                code::UNSUPPORTED_COMPONENT,
                code::VARIABLE_MISSING,
                code::SOURCE_UNSUPPORTED
            ]
        );
        assert!(found.iter().all(|d| !d.is_error()));
    }
}
