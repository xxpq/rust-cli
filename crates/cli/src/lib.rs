//! 云 Harness 的显式组装入口；鉴权、执行环境与产物端口由宿主注入。只在 `cloud` 特性下有（Cargo.toml）：缺省构建不链接
//! PG 会话库。可执行文件 `zcode-rs` 的组装在 `src/main.rs` 那一侧（`assemble`），会话库按部署配置的 `store` 键选。

#[cfg(feature = "cloud")]
pub mod cloud;
