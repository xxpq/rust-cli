//! App 前端层（docs/specs/rust-cli/context.md §15.5，APP-CTX）：App 专有的模型可见内容的唯一来源。
//!
//! 内核经端口 [`FrontendPrompt`] 取内容，不认识 App 的能力与文字。这里持有：`--frontend` 的声明（[`declared`]）、插件 → 要的
//! 前端能力（[`plugins`]）、前端上下文段（[`context`]）、引用与页面状态的写法（[`material`]）、`plugin://` 引用（[`plugin_reference`]）
//! 与它们的文字（[`texts`]，`packages/rust-cli/prompts/app/`）。入口为每套内核装一份：app-server 装 App 的那一份，TUI、`-p`、
//! serve 装「不是 App」的那一份（续写 App 的会话时说 App 的说明不再适用，别的什么都不给）。

mod context;
mod declared;
mod material;
mod plugin_reference;
mod plugins;
mod texts;

use std::sync::Arc;

use rust_cli_base::secret::EnvSnapshot;
use rust_cli_ext::PluginSet;
use rust_cli_ext::plugins::Unhosted;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::frontend_prompt::FrontendPrompt;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::input_extras::{PageState, Quote};
use rust_cli_kernel_api::listing::PluginCapabilities;
use rust_cli_kernel_api::session_prompt::FrontendTraits;
use serde_json::Value;

pub(crate) use declared::{Declared, FRONTEND_FLAG, parse};

/// 这个进程的前端在 App 前端层的样子：入口按它是哪个前端定（组装根只决定装哪一份），内核不知道。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppFrontend {
    /// 不是 App（TUI、`-p`、serve）。
    Absent,
    /// App（v4 与 v5 的 app-server）与它声明的渲染能力；没给 `--frontend` 时什么都没声明。
    App(Renders),
}

/// App 声明的渲染能力（`--frontend` 的 `code-comments`、`file-links`、`gen-ui`）：每一项对应前端上下文段的一节。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Renders {
    /// `::code-comment{…}` 渲染成评审卡（Inline Code Comments 一节）。
    pub(crate) code_comments: bool,
    /// Markdown 链接里的本地文件与本机 URL 可以点开（Files & URLs 一节）。
    pub(crate) file_links: bool,
    /// 内联视图（Interactive views 一节，也是 visualize 的门）：Host 给的输出根目录（执行环境里的绝对路径）；None 是没声明。
    pub(crate) gen_ui: Option<EnvPath>,
}

impl AppFrontend {
    /// app-server 的 `--frontend`：内核认识的能力与 App 的那一份。声明了 `gen-ui` 却没有绝对路径的输出根目录是 Host 的错，
    /// 启动失败（[`declared::declare`]）。
    pub(crate) fn declare(
        declared: &[Declared],
        env: &EnvSnapshot,
    ) -> Result<(FrontendTraits, Self), String> {
        let (traits, renders) = declared::declare(declared, env)?;
        Ok((traits, Self::App(renders)))
    }

    /// 这个前端撑不起来的插件（context.md §15.7）：入口交给 ext，加载时整件跳过。
    pub(crate) fn unhosted(&self) -> Unhosted {
        plugins::unhosted(match self {
            Self::Absent => None,
            Self::App(renders) => Some(renders),
        })
    }

    /// 一套内核的端口实现（context.md §15.5）：Gen UI 一节只在这套内核加载了要它的插件时有（关掉 visualize 时没有）。
    pub(crate) fn prompt(&self, loaded: &PluginSet) -> Arc<dyn FrontendPrompt> {
        Arc::new(match self {
            Self::Absent => AppPrompt {
                plugin_links: false,
                renders: Renders::default(),
            },
            Self::App(renders) => AppPrompt {
                plugin_links: true,
                renders: Renders {
                    gen_ui: renders
                        .gen_ui
                        .clone()
                        .filter(|_| plugins::uses_gen_ui(loaded)),
                    ..renders.clone()
                },
            },
        })
    }
}

/// 端口的实现，一套内核一份。
struct AppPrompt {
    /// App 的输入框写 `plugin://` 链接（`@` 插件）；不是 App 时不解析（context.md §15.11）。
    plugin_links: bool,
    /// 这套内核能宣布的节（Gen UI 已按插件集定好）。
    renders: Renders,
}

impl FrontendPrompt for AppPrompt {
    fn context(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
        announced: Option<&Value>,
    ) -> Option<(String, Value)> {
        context::announce(&self.renders, (workspace, session), announced)
    }

    fn plugin_references(&self, input: &str) -> Vec<String> {
        if self.plugin_links {
            plugin_reference::references(input)
        } else {
            Vec::new()
        }
    }

    fn plugin_reference(&self, found: Vec<PluginCapabilities>) -> Option<(String, Value)> {
        plugin_reference::announce(found)
    }

    fn quotes(&self, quotes: &[Quote]) -> String {
        material::quotes(quotes)
    }

    fn page_states(&self, pages: &[PageState]) -> String {
        material::page_states(pages)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "golden_tests.rs"]
mod golden_tests;
