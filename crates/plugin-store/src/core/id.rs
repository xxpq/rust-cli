//! 插件 id 与名字（plugin-store.md §2.1，PLUGIN-REVIEW K10）：`<name>@<marketplace>`，按最后一个 `@` 切分；插件名与市场名都
//! 符合 `^[a-z0-9][a-z0-9._-]{0,127}$`（Node `PLUGIN_NAME_PATTERN`、`MARKETPLACE_NAME_PATTERN`）。

use std::fmt;

pub use rust_cli_base::names::{INLINE_MARKETPLACE, OFFICIAL_MARKETPLACE};

/// 兼容插件格式的官方市场（`packages/shared/src/plugin-marketplaces.ts` 的缺省市场之一，只登记来源）。
pub const COMPAT_OFFICIAL_MARKETPLACE: &str = "claude-plugins-official";
/// 名字最多这么多个字节。
const MAX_NAME_LEN: usize = 128;

/// 官方的市场 id：只能由同 id 的已知记录刷新得到（Node `isOfficialMarketplaceId` 与 `trustedId`，§9.2）。
pub fn is_official_marketplace(id: &str) -> bool {
    id == OFFICIAL_MARKETPLACE || id == COMPAT_OFFICIAL_MARKETPLACE
}

/// 名字合不合规（插件名与市场名同一条规则）。
pub fn valid_name(name: &str) -> bool {
    let lower_or_digit = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut chars = name.chars();
    chars.next().is_some_and(lower_or_digit)
        && name.len() <= MAX_NAME_LEN
        && chars.all(|c| lower_or_digit(c) || matches!(c, '.' | '_' | '-'))
}

/// 一个插件 id。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId {
    pub name: String,
    pub marketplace: String,
}

impl PluginId {
    pub fn new(name: impl Into<String>, marketplace: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            marketplace: marketplace.into(),
        }
    }

    /// 按最后一个 `@` 切；两边都不能空（Node `parsePluginId`）。
    pub fn parse(text: &str) -> Option<Self> {
        let (name, marketplace) = text.rsplit_once('@')?;
        (!name.is_empty() && !marketplace.is_empty()).then(|| Self::new(name, marketplace))
    }

    /// 依赖的写法：带 `@` 的是完整 id，不带的属于 `marketplace`（Node `qualifyDependency`）。
    pub fn qualify(dependency: &str, marketplace: &str) -> Option<Self> {
        if dependency.contains('@') {
            Self::parse(dependency)
        } else {
            Some(Self::new(dependency, marketplace))
        }
    }

    /// 命令行里的写法：`name` 或 `name@marketplace`（Node `splitPluginIdentifier`）。
    pub fn split_identifier(text: &str) -> (String, Option<String>) {
        match Self::parse(text) {
            Some(id) => (id.name, Some(id.marketplace)),
            None => (text.to_owned(), None),
        }
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.marketplace)
    }
}

#[cfg(test)]
#[path = "id_tests.rs"]
mod tests;
