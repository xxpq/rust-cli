//! 声明的市场（用户级 `plugins.extraKnownMarketplaces`，plugin-store.md §9.5）：照 Node `resolveEffectiveMarketplaceRecords` 一组函数。
//! 声明不写 `known_marketplaces.json`；列出时与已知的记录合在一起，显式刷新、安装时才物化。这里只做合并与判定，不做 IO。

use std::collections::BTreeMap;

use super::diagnostic::{Diagnostic, StoreError, code};
use super::id::is_official_marketplace;
use super::records::Known;
use super::source::MarketplaceSource;

/// 列出用的一条市场：记录，与能不能读它的清单缓存（只有声明、或声明改指了来源的，没有缓存可读）。
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub record: Known,
    pub cached: bool,
}

/// 一条只来自声明的记录（Node `createDeclaredMarketplaceRecord`）：名字是 id，没有清单。
fn declared_record(id: &str, source: &MarketplaceSource) -> Known {
    Known {
        id: id.to_owned(),
        name: id.to_owned(),
        source: source.clone(),
        description: None,
        added_at: None,
        last_updated: None,
        plugin_count: 0,
        refresh_failure: None,
    }
}

/// 已知的记录与声明合并（Node 的规则）：同 id 同来源照记录；同 id 不同来源用声明（官方的 id 照记录、给一条警告）；
/// 只有声明的加一条。声明的官方 id 一律不用。
pub fn merge(
    known: Vec<Known>,
    declared: &BTreeMap<String, MarketplaceSource>,
) -> (Vec<Listed>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let mut listed: Vec<Listed> = known
        .into_iter()
        .map(|record| match declared.get(&record.id) {
            Some(source) if source.raw != record.source.raw => {
                if is_official_marketplace(&record.id) {
                    diagnostics.push(reserved(&record.id));
                    Listed {
                        record,
                        cached: true,
                    }
                } else {
                    Listed {
                        record: declared_record(&record.id, source),
                        cached: false,
                    }
                }
            }
            _ => Listed {
                record,
                cached: true,
            },
        })
        .collect();
    for (id, source) in declared {
        if is_official_marketplace(id) {
            if !listed.iter().any(|item| item.record.id == *id) {
                diagnostics.push(reserved(id));
            }
            continue;
        }
        if !listed.iter().any(|item| item.record.id == *id) {
            listed.push(Listed {
                record: declared_record(id, source),
                cached: false,
            });
        }
    }
    (listed, diagnostics)
}

/// 显式刷新或安装 `id` 之前（Node `materializeDeclaredMarketplaceForExplicitAction`）：要物化的声明；已知记录的来源与声明不同时报错
/// （不改指已有的市场）。没有声明、官方 id、已知且同来源时为 None。
pub fn to_materialize<'a>(
    known: &[Known],
    declared: &'a BTreeMap<String, MarketplaceSource>,
    id: &str,
) -> Result<Option<&'a MarketplaceSource>, StoreError> {
    let Some(source) = declared.get(id).filter(|_| !is_official_marketplace(id)) else {
        return Ok(None);
    };
    match known.iter().find(|record| record.id == id) {
        Some(record) if record.source.raw != source.raw => Err(repointed(id)),
        Some(_) => Ok(None),
        None => Ok(Some(source)),
    }
}

/// 声明改指了已知市场的来源（Node `createMarketplaceSourceRepointDiagnostic` 的文字）。
pub fn repointed(id: &str) -> StoreError {
    StoreError::Invalid(format!(
        "Workspace marketplace declaration \"{id}\" conflicts with an existing Host source. \
         Remove the existing marketplace or use a different marketplace id before materializing it."
    ))
}

/// 声明用了官方的 id（Node `createReservedMarketplaceDeclarationDiagnostic`）。
fn reserved(id: &str) -> Diagnostic {
    Diagnostic::warning(
        code::DECLARATION_RESERVED,
        format!(
            "Workspace marketplace declaration \"{id}\" uses a reserved official id and was ignored. \
             Use a different marketplace id for project declarations."
        ),
    )
    .of(id.to_owned())
}

#[cfg(test)]
#[path = "declared_tests.rs"]
mod tests;
