//! 插件加载的往返次数（ext.md §4.0）：每次 Files 调用有固定延迟（远端机器的一次往返）时，加载的耗时按“串起来的调用层数”
//! 算，不按插件数。时间用 tokio 的暂停时钟，结果是确定的。

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;
use crate::test_support::{RTT, Slow, facts, workspace};

const ROOT: &str = "/Users/dev/plugins";

/// 20 个配置目录插件，每个有 skill、命令、agent、hooks 与 `.mcp.json`；外加 2 个停用的与 1 个不存在的目录。
fn many() -> (MemoryEnvironment, PluginSources) {
    let mut env = MemoryEnvironment::new(facts());
    let mut candidates = Vec::new();
    for index in 0..23 {
        let name = format!("p{index:02}");
        let dir = format!("{ROOT}/{name}");
        candidates.push(PluginCandidate {
            dir: EnvPath::new(&dir),
            marketplace: "inline".to_owned(),
            origin: PluginOrigin::ConfigDir,
        });
        if index == 22 {
            continue;
        }
        for (path, text) in [
            (
                format!("{dir}/.zcode-plugin/plugin.json"),
                format!(r#"{{"name": "{name}"}}"#),
            ),
            (format!("{dir}/skills/s/SKILL.md"), "body\n".to_owned()),
            (format!("{dir}/commands/c.md"), "run\n".to_owned()),
            (format!("{dir}/agents/a.md"), "agent\n".to_owned()),
            (
                format!("{dir}/hooks/hooks.json"),
                r#"{"hooks": {}}"#.to_owned(),
            ),
            (
                format!("{dir}/.mcp.json"),
                r#"{"mcpServers": {"s": {"command": "run"}}}"#.to_owned(),
            ),
        ] {
            env = env.with_file(&path, &text);
        }
    }
    let enabled_plugins = ["p20@inline", "p21@inline"]
        .into_iter()
        .map(|id| (id.to_owned(), false))
        .collect();
    let sources = PluginSources {
        candidates,
        enabled_plugins,
        ..PluginSources::default()
    };
    (env, sources)
}

#[tokio::test(start_paused = true)]
async fn loading_time_follows_the_call_depth_not_the_plugin_count() {
    let (env, sources) = many();
    let expected = load(&env, &workspace(), &[], (&sources, &Unhosted::default())).await;
    let slow = Slow(env);
    let started = tokio::time::Instant::now();
    let loaded = load(&slow, &workspace(), &[], (&sources, &Unhosted::default())).await;
    let took = started.elapsed();
    assert_eq!(loaded.plugins().len(), 20);
    assert_eq!(loaded.id(), expected.id());
    // 串起来的层：根的种类 → manifest 的 stat 与读 → `.mcp.json` 的 stat 与读（第一段）→ 组件（第三段，四类同时读，
    // 最深的是 agent 目录的种类与列目录）。以前逐个插件、逐个组件地读：2.27 s；同一份输入的 id 与以前相同。
    assert!(took <= RTT * 7, "loading took {took:?}");
}
