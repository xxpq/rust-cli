use serde_json::json;

use super::*;
use std::sync::Arc;

use rust_cli_base::plugin::PluginContext;

use crate::plugins::{Plugin, PluginSet, Skipped};

fn kit(manifest: serde_json::Value) -> Plugin {
    Plugin {
        context: Arc::new(PluginContext {
            id: "kit@inline".to_owned(),
            name: "kit".to_owned(),
            ..PluginContext::default()
        }),
        marketplace: "inline".to_owned(),
        manifest,
        ..Plugin::default()
    }
}

fn skipped(id: Option<&str>, reason: SkipReason) -> Skipped {
    Skipped {
        dir: "/Users/dev/plugins/x".to_owned(),
        id: id.map(str::to_owned),
        reason,
    }
}

#[test]
fn loaded_plugins_come_first_then_the_disabled_candidates_and_nothing_else() {
    let loaded = PluginSet::new(
        vec![kit(json!({"name": "kit", "description": "  A kit  "}))],
        vec![
            skipped(Some("off@zcode-plugins-official"), SkipReason::Disabled),
            skipped(Some("twin@other"), SkipReason::DuplicateName),
            skipped(None, SkipReason::ManifestMissing),
        ],
        Vec::new(),
    );
    assert_eq!(
        entries(&loaded),
        [
            PluginEntry {
                id: "kit@inline".to_owned(),
                name: "kit".to_owned(),
                marketplace: "inline".to_owned(),
                enabled: true,
                description: Some("A kit".to_owned()),
                mcp_servers: Vec::new(),
            },
            PluginEntry {
                id: "off@zcode-plugins-official".to_owned(),
                name: "off".to_owned(),
                marketplace: "zcode-plugins-official".to_owned(),
                enabled: false,
                description: None,
                mcp_servers: Vec::new(),
            },
        ]
    );
}

#[test]
fn an_empty_description_is_left_out() {
    let loaded = PluginSet::new(
        vec![kit(json!({"description": "   "}))],
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(entries(&loaded)[0].description, None);
}
