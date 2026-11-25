//! 插件 → 要的前端能力（context.md §15.7）：撑不起来的插件与 Gen UI 一节的开关。

use std::sync::Arc;

use rust_cli_base::plugin::PluginContext;
use rust_cli_ext::Plugin;
use rust_cli_kernel_api::env::EnvPath;

use super::*;

const WHY: &str = "the frontend does not render inline views (Gen UI)";

fn loaded(ids: &[&str]) -> PluginSet {
    let plugins = ids
        .iter()
        .map(|id| Plugin {
            context: Arc::new(PluginContext {
                id: (*id).to_owned(),
                name: id.split('@').next().unwrap().to_owned(),
                ..PluginContext::default()
            }),
            ..Plugin::default()
        })
        .collect();
    PluginSet::new(plugins, Vec::new(), Vec::new())
}

/// 不是 App、App 没声明 `gen-ui`：visualize 撑不起来（带原因）；声明了就没有撑不起来的。
#[test]
fn visualize_is_unhosted_unless_the_app_renders_gen_ui() {
    let visualize = Unhosted::from([(
        "visualize@zcode-plugins-official".to_owned(),
        WHY.to_owned(),
    )]);
    assert_eq!(unhosted(None), visualize);
    assert_eq!(unhosted(Some(&Renders::default())), visualize);
    let gen_ui = Renders {
        gen_ui: Some(EnvPath::new("/Users/dev/.zcode/visualizations")),
        ..Renders::default()
    };
    assert!(unhosted(Some(&gen_ui)).is_empty());
}

/// Gen UI 一节只看官方市场的 visualize 有没有加载：别的市场的同名插件不算。
#[test]
fn only_the_official_visualize_uses_gen_ui() {
    assert!(uses_gen_ui(&loaded(&[
        "demo@inline",
        "visualize@zcode-plugins-official"
    ])));
    assert!(!uses_gen_ui(&loaded(&["visualize@inline", "demo@inline"])));
    assert!(!uses_gen_ui(&PluginSet::default()));
}
