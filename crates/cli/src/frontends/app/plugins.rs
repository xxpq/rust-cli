//! 插件 → 要的前端能力（docs/specs/rust-cli/context.md §15.7）：前端撑不起来的插件不加载（ext 按入口交来的表跳过），这套内核
//! 加载了要 Gen UI 的插件时前端上下文段才有 Interactive views 一节。APP-CTX 从官方表的 `renders` 挪来：「撑不撑得起」随前端变，
//! 官方表随插件变。

use rust_cli_ext::PluginSet;
use rust_cli_ext::plugins::Unhosted;

use super::Renders;
use crate::config::official_plugin_id;

/// 插件要的前端能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    /// 内联视图（Gen UI）。
    GenUi,
}

impl Need {
    fn met(self, renders: &Renders) -> bool {
        match self {
            Self::GenUi => renders.gen_ui.is_some(),
        }
    }

    /// 跳过时日志里的原因。
    fn why(self) -> &'static str {
        match self {
            Self::GenUi => "the frontend does not render inline views (Gen UI)",
        }
    }
}

/// 要前端能力才有意义的官方插件 → 要的能力（A6）：以前是 ext 里按名字挡掉 visualize 的一个常量（终端不加载、App 也不加载）；
/// 现在按前端声明的能力判，终端不是 App 照旧不加载，App 声明了 `gen-ui` 时加载（仍要启用：它不在缺省启用名单里）。
const NEEDS: [(&str, Need); 1] = [("visualize", Need::GenUi)];

/// 前端撑不起来的插件（id → 原因）。`renders` 为 None：不是 App，表里的都撑不起来。
pub(super) fn unhosted(renders: Option<&Renders>) -> Unhosted {
    NEEDS
        .iter()
        .filter(|(_, need)| !renders.is_some_and(|renders| need.met(renders)))
        .map(|(name, need)| (official_plugin_id(name), need.why().to_owned()))
        .collect()
}

/// 这套内核加载好的插件里有要 Gen UI 的（同 Node：插件关时整节省略）。只认官方市场的 id：别的市场的同名插件不算。
pub(super) fn uses_gen_ui(loaded: &PluginSet) -> bool {
    let wanted: Vec<String> = NEEDS
        .iter()
        .filter(|(_, need)| *need == Need::GenUi)
        .map(|(name, _)| official_plugin_id(name))
        .collect();
    loaded
        .plugins()
        .iter()
        .any(|plugin| wanted.iter().any(|id| id == plugin.id()))
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
