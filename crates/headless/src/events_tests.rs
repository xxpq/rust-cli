//! 被中断的回合的 result（headless.md §3.6）：中止发生在工具执行之后还是读流时，`terminal_reason` 与诊断不同。

use std::collections::BTreeMap;

use rust_cli_kernel_api::message::{StopReason, Usage};

use super::*;

fn interrupted(after_tools: bool, stop_reason: Option<StopReason>) -> Value {
    let models = BTreeMap::new();
    turn_result(TurnResult {
        duration_api_ms: 0,
        stop_reason,
        session_id: "sess_1",
        usage: Usage::default(),
        model_usage: &models,
        num_turns: 1,
        kind: ResultKind::Interrupted { after_tools },
        duration_ms: 0,
        uuid: "uuid_1".to_owned(),
        result_index: 0,
    })
}

/// 修前一律写 `aborted_streaming` 与 `stop_reason=null`：只对读流时被中断（SIGINT）成立。工具执行之后被中止（PermissionRequest hook 的
/// `interrupt`，hooks.md §6.2）时是 `aborted_tools`，诊断里是最后一条回复的 `stop_reason`。
#[test]
fn an_abort_after_the_tools_is_aborted_tools() {
    let result = interrupted(true, Some(StopReason::ToolUse));
    assert_eq!(result["subtype"], "error_during_execution");
    assert_eq!(result["terminal_reason"], "aborted_tools");
    assert_eq!(
        result["errors"][0],
        "[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"
    );
    let result = interrupted(false, None);
    assert_eq!(result["terminal_reason"], "aborted_streaming");
    assert_eq!(
        result["errors"][0],
        "[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=null"
    );
}

/// system/init 的插件（ext.md §4.14，PLUG-P0）：`plugins` 在 `skills` 之后，没有版本时不写 `version`；`plugin_errors` 只在有错误时写，
/// uuid 总在最后。旧代码两个键都不写。
#[test]
fn init_lists_the_plugins_and_their_errors() {
    use rust_cli_kernel_api::catalog::{PluginInfo, PluginLoadError, SessionCatalog};
    let catalog = SessionCatalog {
        plugins: vec![
            PluginInfo {
                name: "demo".to_owned(),
                path: "/work/plugin".to_owned(),
                source: "demo@inline".to_owned(),
                version: Some("1.0.0".to_owned()),
            },
            PluginInfo {
                name: "bare".to_owned(),
                path: "/work/bare".to_owned(),
                source: "bare@inline".to_owned(),
                version: None,
            },
        ],
        plugin_errors: vec![PluginLoadError {
            plugin: "inline[2]".to_owned(),
            kind: "path-not-found".to_owned(),
            message: "the plugin root does not exist".to_owned(),
            path: Some("/work/gone".to_owned()),
        }],
        ..SessionCatalog::default()
    };
    let event = init(
        &Init {
            cwd: "/work",
            session_id: "sess_1",
            catalog: &catalog,
            mcp_servers: &[],
            model: "m",
            permission_mode: "default",
            api_key_source: "none",
            version: "0.1.0",
        },
        "uuid_1",
    );
    let keys: Vec<&str> = event
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        &keys[keys.len() - 4..],
        ["skills", "plugins", "plugin_errors", "uuid"]
    );
    assert_eq!(
        event["plugins"],
        serde_json::json!([
            {"name": "demo", "path": "/work/plugin", "source": "demo@inline", "version": "1.0.0"},
            {"name": "bare", "path": "/work/bare", "source": "bare@inline"}
        ])
    );
    assert_eq!(
        event["plugin_errors"],
        serde_json::json!([{"plugin": "inline[2]", "type": "path-not-found", "message": "the plugin root does not exist", "path": "/work/gone"}])
    );
}
