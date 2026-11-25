//! Node CLI 会话库的记录格式与编排（docs/specs/rust-cli/node-store.md；F128，审查 X5；pg-store.md §12）。
//!
//! 记录格式是纯函数：内核的事实与消息 ↔ Node 的 message / part / session_entry 的 JSON，活动分支与压缩边界，冷读取的
//! 消息重建。编排（写入、冷读取、fork、查询）只经 [`rows::Rows`] 读写行：本机 node-store（SQLite）与云端 pg-store
//! （PostgreSQL）各自实现它并负责事务，两边写出的记录逐字节相同。
//!
//! - [`records`]：消息、part、entry 的形状（Node 键序）；[`keys`]：本工程独有的 entry 的名字与键；
//! - [`codecs`]：行的 `data` 列 ↔ 带 id 的 JSON；[`stored`]：要写的 message / part 行的列；[`row`]：一条消息与它的 part；
//! - [`branch`]：活动分支（回退游标、压缩边界）；[`hydrate`]：Node 记录 → 内核消息；
//! - [`tool_parts`] / [`tool_content`]：工具调用与结果的 part；[`notices`]：本工程写的合成消息；[`compaction`]：压缩边界；
//! - [`mode`]：权限模式与 Node 的协作模式；[`artifacts`]：artifact 的 URI；[`subagents`]：Agent 调用关联到的子会话；
//! - 编排：[`rows`]（行操作）、[`append`] / [`facts`]（提交）、[`load`]（冷读取）、[`fork`] / [`fork_clone`]、[`query`]、
//!   [`side_chat`]、[`timeline`]、[`session_facts`]、[`rewind`]、[`usage`]、本机设置（[`project_rules`]、[`project_mode`]、[`workspace_trust`]）。
use std::fmt::Display;

use rust_cli_kernel_api::store::StoreError;

pub mod append;
pub mod artifacts;
pub mod branch;
pub mod codecs;
pub mod compaction;
pub mod facts;
pub mod fork;
pub mod fork_clone;
pub mod frontend_records;
pub mod hydrate;
pub mod ids;
pub mod keys;
pub mod load;
pub mod mode;
pub mod notices;

pub mod project_mode;
pub mod project_rules;
pub mod query;
pub mod range;
pub mod raw;
pub mod records;
pub mod rewind;
pub mod row;
pub mod rows;
pub mod rust_entries;
pub mod session_facts;
pub mod side_chat;
pub mod state;
pub mod stored;
pub mod subagents;
pub mod timeline;
pub mod tool_content;
pub mod tool_parts;
pub mod usage;
pub mod workspace_trust;

/// 本机执行环境的 ID（Node 的本地会话 `workspace_id` 为 NULL）。
pub const LOCAL_ENVIRONMENT: &str = "local";

/// 别的错误（JSON 解析、后端自己的驱动错误）换成 `StoreError::Storage`。行操作（[`rows::Rows`]）的结果已经是分好类的
/// `StoreError`，直接 `?`，不要再经这里：会把 `Unavailable` 这样的类别抹成 `Storage`。
pub fn storage(error: impl Display) -> StoreError {
    StoreError::Storage {
        message: error.to_string(),
    }
}
