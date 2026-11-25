//! App 前端层的端口（context.md §15.5）：按入口装的那一份决定给什么。

use std::sync::Arc;

use rust_cli_base::plugin::PluginContext;
use rust_cli_ext::Plugin;
use rust_cli_kernel_api::ids::EnvironmentId;

use super::*;

const ROOT: &str = "/Users/dev/.zcode/visualizations";
const LINK: &str = "use [Kit](plugin://kit@market)";

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/work"),
    }
}

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

fn context(prompt: &dyn FrontendPrompt, announced: Option<&Value>) -> Option<(String, Value)> {
    prompt.context(&workspace(), &SessionId::new("sess_1"), announced)
}

/// 不是 App（TUI、`-p`、serve）：不解析 `plugin://` 链接，前端上下文段从不宣布；续写 App 的会话时只说不再适用。
#[test]
fn a_frontend_that_is_not_the_app_only_withdraws() {
    let absent = AppFrontend::Absent.prompt(&loaded(&["visualize@zcode-plugins-official"]));
    assert!(absent.plugin_references(LINK).is_empty());
    assert_eq!(context(absent.as_ref(), None), None);
    let announced = serde_json::json!({"file_links": true});
    let (text, data) = context(absent.as_ref(), Some(&announced)).unwrap();
    assert_eq!(text, texts::frontend_gone());
    assert_eq!(data, serde_json::json!({}));
}

/// App 解析 `plugin://` 链接，没给 `--frontend`（什么都没声明）时也解析；没声明渲染能力时不宣布前端上下文段。
#[test]
fn the_app_parses_plugin_links_without_declarations() {
    let app = AppFrontend::App(Renders::default()).prompt(&PluginSet::default());
    assert_eq!(app.plugin_references(LINK), ["kit@market"]);
    assert_eq!(context(app.as_ref(), None), None);
}

/// Gen UI 一节要两样：App 声明了 `gen-ui`，且这套内核加载了要它的插件（关掉 visualize 时没有这一节，也就没有目录）。
#[test]
fn the_gen_ui_section_needs_the_declaration_and_the_plugin() {
    let declared = AppFrontend::App(Renders {
        file_links: true,
        gen_ui: Some(EnvPath::new(ROOT)),
        ..Renders::default()
    });
    let on = declared.prompt(&loaded(&["visualize@zcode-plugins-official"]));
    let (text, _) = context(on.as_ref(), None).unwrap();
    assert!(text.contains("### Interactive views"), "{text}");
    assert!(text.contains(ROOT), "{text}");
    let off = declared.prompt(&loaded(&["demo@inline"]));
    let (text, data) = context(off.as_ref(), None).unwrap();
    assert!(!text.contains("Interactive views"), "{text}");
    assert!(!text.contains(ROOT), "{text}");
    assert_eq!(data, serde_json::json!({"file_links": true}));
    let undeclared = AppFrontend::App(Renders {
        file_links: true,
        ..Renders::default()
    })
    .prompt(&loaded(&["visualize@zcode-plugins-official"]));
    let (text, _) = context(undeclared.as_ref(), None).unwrap();
    assert!(!text.contains("Interactive views"), "{text}");
}

/// 撑不起来的插件随入口：不是 App 与没声明 `gen-ui` 的 App 都跳过 visualize。
#[test]
fn unhosted_plugins_follow_the_frontend() {
    let visualize = "visualize@zcode-plugins-official";
    assert!(AppFrontend::Absent.unhosted().contains_key(visualize));
    assert!(
        AppFrontend::App(Renders::default())
            .unhosted()
            .contains_key(visualize)
    );
    let gen_ui = AppFrontend::App(Renders {
        gen_ui: Some(EnvPath::new(ROOT)),
        ..Renders::default()
    });
    assert!(gen_ui.unhosted().is_empty());
}

/// 文字都在：`prompts/app/` 下每个文件都编进来了、都不空。
#[test]
fn every_app_text_is_present() {
    for (file, text) in texts::ALL {
        assert!(!text().is_empty(), "{file}");
        assert!(!text().ends_with('\n'), "{file}");
    }
}
