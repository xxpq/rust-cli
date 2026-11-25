//! `@` 插件的引用（`plugin_reference`，docs/specs/rust-cli/context.md §15.11；照 Node `core/src/plugin-reference/references.ts`、
//! `reminder.ts`）：人类键入的输入里有 `[label](plugin://<id>)` 时，给模型一段只有标识符的固定模板，说这几个插件这一轮有哪些
//! skill、MCP 服务器与 agent。插件给的任何文字都不进来；身份只来自链接的 destination，label 不参与。
//!
//! 槽位 A5（输入材料）：user 角色、`<system-reminder>` 包装、落库，查插件这一刻的能力，都在内核（context）；这里只解析与写模板。
//! 只有 App 解析（App 的输入框写这种链接，§15.11）。APP-CTX 从 context crate 搬来。

use regex::Regex;
use rust_cli_kernel_api::listing::PluginCapabilities;
use rust_cli_prompts::texts::render;
use serde_json::{Value, json};
use std::sync::OnceLock;

use super::texts;

/// Markdown 链接（Node `MARKDOWN_LINK_PATTERN`）：label 支持 `\` 转义，destination 是 `<…>` 或裸写。
fn markdown_link() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"\[(?:\\.|[^\\\]])*\]\((?:<((?:\\.|[^>])*?)>|((?:\\.|[^)\s])*))\)")
            .unwrap_or_else(|_| unreachable!())
    })
}
/// 协议名只认小写（Node `PLUGIN_REFERENCE_SCHEME`）。
const SCHEME: &str = "plugin://";
/// 一轮最多几个引用（Node `MAX_PLUGIN_REFERENCES_PER_TURN`）。
const MAX_REFERENCES: usize = 8;
/// 插件 id 的长度上限（Node `MAX_PLUGIN_STABLE_ID_LENGTH`）。
const MAX_ID_LEN: usize = 256;
/// 能力标识符的长度上限（Node `MAX_CAPABILITY_IDENTIFIER_LENGTH`）。
const MAX_IDENTIFIER_LEN: usize = 128;
/// 上限（Node `MAX_PLUGIN_REFERENCE_SKILLS` / `_MCP_SERVERS` / `_SUBAGENTS` / `_REMINDER_BYTES`），按引用的顺序用掉。
const MAX_SKILLS: usize = 32;
const MAX_MCP_SERVERS: usize = 16;
const MAX_SUBAGENTS: usize = 16;
const MAX_BODY_BYTES: usize = 8 * 1024;

/// 插件 id 的一段（Node `PLUGIN_ID_SEGMENT_PATTERN`：`^[A-Za-z0-9][A-Za-z0-9._-]*$`）。
fn valid_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 严格的插件 id：`<name>@<marketplace>`，只有一个 `@`（Node `isValidPluginStableId`）。
fn valid_id(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.len() > MAX_ID_LEN {
        return false;
    }
    match candidate.split_once('@') {
        Some((name, marketplace)) => {
            !marketplace.contains('@') && valid_segment(name) && valid_segment(marketplace)
        }
        None => false,
    }
}

/// 能力标识符（Node `CAPABILITY_IDENTIFIER_PATTERN`：`^[A-Za-z0-9._:@/-]+$`，不超过 128 个字符）。
fn valid_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAX_IDENTIFIER_LEN
        && identifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '/' | '-'))
}

/// 输入里引用的插件 id（Node `extractPluginReferences`）：按首次出现的顺序去重，最多 8 个；协议不对、id 不合格的不算。
pub(super) fn references(input: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for link in markdown_link().captures_iter(input) {
        let destination = link
            .get(1)
            .or_else(|| link.get(2))
            .map_or("", |part| part.as_str());
        let Some(id) = destination.strip_prefix(SCHEME).filter(|id| valid_id(id)) else {
            continue;
        };
        if found.iter().any(|seen| seen == id) {
            continue;
        }
        if found.len() == MAX_REFERENCES {
            break;
        }
        found.push(id.to_owned());
    }
    found
}

/// 名字列表的写法（Node：`JSON.stringify` 每个名字，`, ` 连接）。
fn names(list: &[String]) -> String {
    list.iter()
        .map(|name| json!(name).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn body(plugins: &[PluginCapabilities]) -> String {
    let lines: Vec<String> = plugins
        .iter()
        .map(|plugin| {
            render(
                texts::plugin_reference_plugin(),
                &[
                    ("id", &json!(plugin.id).to_string()),
                    ("skills", &names(&plugin.skills)),
                    ("mcp_servers", &names(&plugin.mcp_servers)),
                    ("subagents", &names(&plugin.subagents)),
                ],
            )
        })
        .collect();
    render(texts::plugin_reference(), &[("plugins", &lines.join("\n"))])
}

/// 一类能力按剩下的预算取（Node：按引用的顺序用掉上限）。
fn within(list: Vec<String>, budget: &mut usize) -> Vec<String> {
    let kept: Vec<String> = list
        .into_iter()
        .filter(|name| valid_identifier(name))
        .take(*budget)
        .collect();
    *budget -= kept.len();
    kept
}

/// 提醒的 `(text, data)`（text 未包 `<system-reminder>`）。`found` 是插件目录给的能力，按引用的顺序；什么能力都没有的插件跳过，
/// 全部跳过时为 None（Node：整条省略）。
pub(super) fn announce(found: Vec<PluginCapabilities>) -> Option<(String, Value)> {
    let (mut skills, mut servers, mut agents) = (MAX_SKILLS, MAX_MCP_SERVERS, MAX_SUBAGENTS);
    let mut kept: Vec<PluginCapabilities> = Vec::new();
    for plugin in found {
        let plugin = PluginCapabilities {
            skills: within(plugin.skills, &mut skills),
            mcp_servers: within(plugin.mcp_servers, &mut servers),
            subagents: within(plugin.subagents, &mut agents),
            id: plugin.id,
        };
        if plugin.skills.is_empty() && plugin.mcp_servers.is_empty() && plugin.subagents.is_empty()
        {
            continue;
        }
        kept.push(plugin);
    }
    // 字节上限：从尾部整条去掉插件（保留引用顺序在前的）。
    while !kept.is_empty() && body(&kept).len() > MAX_BODY_BYTES {
        kept.pop();
    }
    if kept.is_empty() {
        return None;
    }
    let ids: Vec<&str> = kept.iter().map(|plugin| plugin.id.as_str()).collect();
    Some((body(&kept), json!({ "plugins": ids })))
}

#[cfg(test)]
#[path = "plugin_reference_tests.rs"]
mod tests;
