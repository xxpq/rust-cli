//! 前端层端口的替身（[`crate::frontend_prompt::FrontendPrompt`]）：内核的测试只看槽位、位置、包装与角色，文字是这里的回声，
//! 不是任何前端的文字。

use serde_json::{Value, json};

use crate::env::WorkspaceRef;
use crate::frontend_prompt::FrontendPrompt;
use crate::ids::SessionId;
use crate::input_extras::{PageState, Quote};
use crate::listing::PluginCapabilities;

/// 输入里 `plugin:<id>` 这样的词算一个引用。
const REFERENCE_PREFIX: &str = "plugin:";

/// 回声：前端上下文段按会话 ID 写一次（`data` 相同不再写）；`plugin:<id>` 是引用；引用与页面状态写成一行。
#[derive(Debug, Clone, Copy, Default)]
pub struct EchoFrontend;

impl FrontendPrompt for EchoFrontend {
    fn context(
        &self,
        _workspace: &WorkspaceRef,
        session: &SessionId,
        announced: Option<&Value>,
    ) -> Option<(String, Value)> {
        let data = json!({ "session": session.as_str() });
        (announced != Some(&data)).then(|| (format!("frontend of {}", session.as_str()), data))
    }

    fn plugin_references(&self, input: &str) -> Vec<String> {
        input
            .split_whitespace()
            .filter_map(|word| word.strip_prefix(REFERENCE_PREFIX))
            .map(str::to_owned)
            .collect()
    }

    fn plugin_reference(&self, found: Vec<PluginCapabilities>) -> Option<(String, Value)> {
        let ids: Vec<String> = found.into_iter().map(|plugin| plugin.id).collect();
        (!ids.is_empty()).then(|| {
            (
                format!("referenced {}", ids.join(", ")),
                json!({ "plugins": ids }),
            )
        })
    }

    fn quotes(&self, quotes: &[Quote]) -> String {
        let texts: Vec<&str> = quotes.iter().map(|quote| quote.text.as_str()).collect();
        format!("quoted {}", texts.join(" | "))
    }

    fn page_states(&self, pages: &[PageState]) -> String {
        let paths: Vec<&str> = pages.iter().map(|page| page.path.as_str()).collect();
        format!("pages {}", paths.join(" | "))
    }
}
