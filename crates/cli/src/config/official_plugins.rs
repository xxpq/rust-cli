//! 官方插件表（docs/specs/rust-cli/ext.md §4.3、§4.4）：入口层的数据，作为 `PluginSources` 的值交给 ext，内核里没有插件名。
//!
//! 内容以 Node 为准：缺省启用的是 `packages/shared/src/plugin-marketplaces.ts` 的 `DEFAULT_ENABLED_OFFICIAL_PLUGIN_IDS`（与 Node
//! `official-plugin-definitions.ts` 里标了 `defaultEnabled` 的项一致）。另两项是 rust 不加载的、依赖 Node 宿主的插件（§4.4），
//! 依赖宿主的官方插件不依赖宿主之后（§4.8）删掉。要前端能力的插件不在这里：那张表随前端变，在 App 前端层
//! （`frontends/app/plugins.rs`，context.md §15.7）。

use rust_cli_base::names::OFFICIAL_MARKETPLACE;
use rust_cli_ext::plugins::OfficialTable;

/// 缺省启用的官方插件。修 bug（PLUG-P0，PLUGIN-REVIEW F07）：以前 ext 里写死的名单多 `visualize`、少 `zcode-site`，与 Node
/// 不同；开关开时设置页（Node 的泳道判定）显示「已启用」的插件，rust 的会话里却没有。
const DEFAULT_ENABLED: [&str; 11] = [
    "browser-use",
    "image-search",
    "documents",
    "pdf",
    "presentations",
    "spreadsheets",
    "node-repl-host",
    "skill-creator",
    "plugin-creator",
    "zcode-guide",
    "zcode-site",
];

/// 能力来自 Node 宿主注入的 `node_repl` MCP 的官方插件（Node `hostMcpServerNames` 与宿主本身）。
const NODE_REPL: [&str; 3] = ["node-repl-host", "browser-use", "computer-use"];

/// MCP 要 Node 运行时的官方插件：Node 播种缓存时把它们改写成经 Node CLI 自身的插件宿主启动；安装包里的原样 manifest 写的是
/// `command: node`（§4.4 第二行）。以前按缓存里的改写参数判断，内置目录里的原样插件判断不出。
const NODE_RUNTIME: [&str; 6] = [
    "android-emulator",
    "ios-simulator",
    "documents",
    "pdf",
    "presentations",
    "spreadsheets",
];

/// 官方市场的插件 id（`<名字>@zcode-plugins-official`）：App 前端层的「插件 → 要的前端能力」表也用它。
pub(crate) fn id(name: &str) -> String {
    format!("{name}@{OFFICIAL_MARKETPLACE}")
}

fn ids(names: &[&str]) -> std::collections::BTreeSet<String> {
    names.iter().map(|name| id(name)).collect()
}

pub(crate) fn table() -> OfficialTable {
    OfficialTable {
        default_enabled: ids(&DEFAULT_ENABLED),
        node_repl: ids(&NODE_REPL),
        node_runtime: ids(&NODE_RUNTIME),
    }
}
