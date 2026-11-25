//! 依赖闭包（plugin-store.md §5.1，Node `resolveDependencyClosure`）：深度优先，依赖排在前面；跨市场的依赖只在根市场清单的
//! `allowCrossMarketplaceDependenciesOn` 里列了时允许；有环、缺依赖报错。文字照 Node。

use super::catalog::{Catalog, Entry};
use super::diagnostic::StoreError;
use super::id::PluginId;

/// 闭包里的一项：id 与它在清单里的条目。
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub id: PluginId,
    pub entry: Entry,
}

/// `root` 的依赖闭包。`catalog` 按市场名取清单（没有这个市场时为 None）。
pub fn closure<'a>(
    root: &PluginId,
    catalog: impl Fn(&str) -> Option<&'a Catalog>,
) -> Result<Vec<Member>, StoreError> {
    let allowed = catalog(&root.marketplace)
        .map(|catalog| catalog.cross_marketplace.clone())
        .unwrap_or_default();
    let mut walk = Walk {
        root: root.clone(),
        allowed,
        catalog: &catalog,
        visiting: Vec::new(),
        members: Vec::new(),
    };
    walk.visit(root, &root.to_string())?;
    Ok(walk.members)
}

struct Walk<'c, F> {
    root: PluginId,
    allowed: Vec<String>,
    catalog: &'c F,
    visiting: Vec<PluginId>,
    members: Vec<Member>,
}

impl<'a, F: Fn(&str) -> Option<&'a Catalog>> Walk<'_, F> {
    fn visit(&mut self, id: &PluginId, required_by: &str) -> Result<(), StoreError> {
        if id.marketplace != self.root.marketplace && !self.allowed.contains(&id.marketplace) {
            return Err(StoreError::CrossMarketplace(format!(
                "Cross-marketplace dependency is blocked: {id} required by {required_by}"
            )));
        }
        if self.visiting.contains(id) {
            let chain: Vec<String> = self
                .visiting
                .iter()
                .chain([id])
                .map(ToString::to_string)
                .collect();
            return Err(StoreError::DependencyCycle(format!(
                "Plugin dependency cycle: {}",
                chain.join(" -> ")
            )));
        }
        if self.members.iter().any(|member| &member.id == id) {
            return Ok(());
        }
        let is_root = *id == self.root;
        let Some(catalog) = (self.catalog)(&id.marketplace) else {
            return Err(if is_root {
                StoreError::MarketplaceNotFound(id.marketplace.clone())
            } else {
                StoreError::DependencyMissing(format!(
                    "Marketplace not found for dependency: {}",
                    id.marketplace
                ))
            });
        };
        let Some(entry) = catalog.find(&id.name) else {
            return Err(if is_root {
                StoreError::PluginNotFound(id.to_string())
            } else {
                StoreError::DependencyMissing(format!(
                    "Dependency not found: {id} required by {required_by}"
                ))
            });
        };
        self.visiting.push(id.clone());
        let required = id.to_string();
        for dependency in &entry.dependencies {
            let Some(dependency) = PluginId::qualify(dependency, &id.marketplace) else {
                continue;
            };
            self.visit(&dependency, &required)?;
        }
        self.visiting.pop();
        self.members.push(Member {
            id: id.clone(),
            entry: entry.clone(),
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "closure_tests.rs"]
mod tests;
