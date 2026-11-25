use rust_cli_agents::builtins;

use super::*;

fn entry(name: &str, enabled: bool) -> PluginEntry {
    PluginEntry {
        id: format!("{name}@inline"),
        name: name.to_owned(),
        marketplace: "inline".to_owned(),
        enabled,
        description: None,
        mcp_servers: Vec::new(),
    }
}

fn agent(agent_type: &str, source: AgentSource) -> AgentDefinition {
    AgentDefinition {
        agent_type: agent_type.to_owned(),
        source,
        ..builtins().remove(0)
    }
}

#[test]
fn agents_are_sorted_into_the_plugin_their_type_names() {
    let agents = [
        agent("kit:team:lead", AgentSource::Plugin),
        agent("kit:reviewer", AgentSource::Plugin),
        // 另一个插件名以它开头：按整段比，不按前缀
        agent("kit-two:helper", AgentSource::Plugin),
        // 项目里同名的 agent 不算插件的
        agent("kit:local", AgentSource::Project),
    ];
    let built = build(
        vec![
            entry("kit", true),
            entry("kit-two", true),
            entry("off", false),
        ],
        &[],
        &agents,
    );
    let subagents: Vec<(&str, Vec<&str>)> = built
        .iter()
        .map(|reference| {
            (
                reference.entry.name.as_str(),
                reference.subagents.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        subagents,
        [
            ("kit", vec!["kit:reviewer", "kit:team:lead"]),
            ("kit-two", vec!["kit-two:helper"]),
            ("off", vec![]),
        ]
    );
    assert!(built.iter().all(|reference| reference.skills.is_empty()));
}

/// `plugin_reference` 的能力（context.md §15.11）：MCP 服务器只留这一步工具里有它的工具的（没连上、工具被禁用的不算），按引用的顺序。
#[test]
fn referenced_plugins_list_only_visible_mcp_servers_in_reference_order() {
    let with_servers = |name: &str, servers: &[&str]| PluginEntry {
        mcp_servers: servers.iter().map(|server| (*server).to_owned()).collect(),
        ..entry(name, true)
    };
    let references = build(
        vec![
            with_servers("kit", &["plugin:kit:db", "plugin:kit:off"]),
            with_servers("doc", &[]),
        ],
        &[],
        &[],
    );
    let tools = vec![
        "Read".to_owned(),
        rust_cli_kernel_api::mcp_name::compose("plugin:kit:db", "query"),
    ];
    let ids = vec!["doc@inline".to_owned(), "kit@inline".to_owned()];
    let found = capabilities(references, &tools, &ids);
    assert_eq!(
        found
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect::<Vec<_>>(),
        ["doc@inline", "kit@inline"]
    );
    assert_eq!(found[1].mcp_servers, ["plugin:kit:db"]);
}
