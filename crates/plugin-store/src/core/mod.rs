//! 领域核心（plugin-store.md §2.1）：只有值与纯函数，不做 IO。

pub mod catalog;
pub mod closure;
pub mod config_patch;
pub mod declared;
pub mod diagnostic;
pub mod digest;
pub mod id;
pub mod manifest;
pub mod records;
pub mod secrets;
pub mod source;
pub mod validate;

/// 孤儿目录过多久删（14 天，plugin-store.md §6.3）。
pub const ORPHAN_TTL_MS: u64 = 14 * 24 * 60 * 60 * 1000;
/// Node `DEFAULT_VERSION`：manifest 与条目都没写版本时。
pub const DEFAULT_VERSION: &str = "0.0.0";

/// `JSON.stringify(value, null, 2)` 加换行：Node 写这些文件的写法（记录、市场清单、私有文件、标记）。
pub fn json_text(value: &serde_json::Value) -> String {
    let mut text = rust_cli_base::js::json::pretty(value);
    text.push('\n');
    text
}
