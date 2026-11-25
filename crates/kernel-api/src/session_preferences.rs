//! 宿主给的会话偏好与冷打开时的起始状态（docs/specs/rust-cli/app-prefs.md §4.2）。
//!
//! App 里偏好的 owner 是宿主的设置；前端在建会话、冷打开之前取一次，交给内核放进会话的起始状态。之后会话 actor 是它在内核里的
//! 唯一 owner（只在内存、打开着时不变），每次运行的请求带一份只读的副本：主循环、派生的子代理、压缩的摘要请求。

use serde::{Deserialize, Serialize};

use crate::env::EnvPath;

/// 会话偏好：只能关掉内核有的能力，不能打开内核关掉的（`ZCODE_DISABLE_AUTO_MEMORY`、`ENABLE_TOOL_SEARCH` 为关时仍然没有）。
/// 缺省什么都不关（TUI、`-p`、v4 的 App 不给）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionPreferences {
    /// 用不用自动记忆：假时这个会话的请求没有记忆段与 MEMORY.md。
    pub auto_memory: bool,
    /// 用不用 ToolSearch：假时这个会话的运行里没有 ToolSearch（与 `ENABLE_TOOL_SEARCH` 为关走同一个分支）。
    pub tool_search: bool,
}

impl Default for SessionPreferences {
    fn default() -> Self {
        Self {
            auto_memory: true,
            tool_search: true,
        }
    }
}

impl SessionPreferences {
    /// 什么都不关（序列化时省掉）。
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// 这次运行的禁用名单：`disallowed`（输入带的、派生它的运行的）加上偏好关掉的工具，不重复。
    pub fn disallowed(&self, disallowed: &[String]) -> Vec<String> {
        let mut names = disallowed.to_vec();
        if !self.tool_search
            && !names
                .iter()
                .any(|name| name == crate::tool_names::TOOL_SEARCH)
        {
            names.push(crate::tool_names::TOOL_SEARCH.to_owned());
        }
        names
    }
}

/// 冷打开时调用方给的起始状态（[`crate::session_api::Kernel::open_session_with`]；App 里是宿主给的）。会话已经打开着时不起作用
/// （运行实例存在时不刷新）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenStart {
    /// 接在内核缺省之后的附加工作目录（docs/specs/rust-cli/session-dirs.md §3.1、§12.4）。
    pub directories: Vec<EnvPath>,
    pub preferences: SessionPreferences,
}

#[cfg(test)]
#[path = "session_preferences_tests.rs"]
mod tests;
