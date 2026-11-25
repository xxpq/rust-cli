//! 能力协商（03 §1、§3.3；与 `packages/protocol/src/caps/capabilities.ts` 的 `negotiate` / `upgradeTarget` 同一规则）：逐能力取双方共有的最高修订；`require` 里协商不出来的 → 拒绝，按修订区间判断哪端该升级。

use std::collections::BTreeMap;

use crate::consts::SERVED_CAPABILITIES;
use crate::generated::contract::CAPABILITIES;
use crate::generated::core_v1::{InitializeRejectionMissingItem, InitializeRejectionUpgradeTarget};

/// 一端声明的能力：能力 → 支持的修订。
pub type Declaration = BTreeMap<String, Vec<u64>>;
/// 协商结果：能力 → 选定的修订。
pub type Negotiated = BTreeMap<String, u64>;

/// 本端总是提供的能力与修订（取生成的能力表）。
pub fn served() -> Declaration {
    declared_of(|id| SERVED_CAPABILITIES.contains(&id))
}

/// 一项能力按生成的能力表的修订声明（入口交了端口才声明的那几项，例如插件管理，protocol-v5.md §72.1）。
pub fn declared(capability: &str) -> Declaration {
    declared_of(|id| id == capability)
}

fn declared_of(pick: impl Fn(&str) -> bool) -> Declaration {
    CAPABILITIES
        .iter()
        .filter(|cap| pick(cap.id))
        .map(|cap| {
            (
                cap.id.to_owned(),
                cap.revisions.iter().map(|&rev| u64::from(rev)).collect(),
            )
        })
        .collect()
}

fn highest_common(mine: &[u64], theirs: &[u64]) -> Option<u64> {
    mine.iter()
        .filter(|rev| theirs.contains(rev))
        .max()
        .copied()
}

/// 协商不出来的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub cap: String,
    pub mine: Vec<u64>,
    pub theirs: Vec<u64>,
}

impl Missing {
    pub fn to_wire(&self) -> InitializeRejectionMissingItem {
        let signed = |revs: &[u64]| {
            revs.iter()
                .map(|&rev| i64::try_from(rev).unwrap_or(i64::MAX))
                .collect()
        };
        InitializeRejectionMissingItem {
            cap: self.cap.clone(),
            mine: signed(&self.mine),
            theirs: signed(&self.theirs),
        }
    }
}

/// 由服务端调用：`mine` 是本端声明，`theirs` 与 `required` 来自 initialize。
pub fn negotiate(
    mine: &Declaration,
    theirs: &Declaration,
    required: &[String],
) -> Result<Negotiated, (Vec<Missing>, InitializeRejectionUpgradeTarget)> {
    let none = Vec::new();
    let mut missing = Vec::new();
    let mut caps = Negotiated::new();
    for cap in required {
        let m = mine.get(cap).unwrap_or(&none);
        let t = theirs.get(cap).unwrap_or(&none);
        match highest_common(m, t) {
            Some(rev) => {
                caps.insert(cap.clone(), rev);
            }
            None => missing.push(Missing {
                cap: cap.clone(),
                mine: m.clone(),
                theirs: t.clone(),
            }),
        }
    }
    if !missing.is_empty() {
        let upgrade = upgrade_target(&missing);
        return Err((missing, upgrade));
    }
    for (cap, m) in mine {
        if caps.contains_key(cap) {
            continue;
        }
        if let Some(rev) = highest_common(m, theirs.get(cap).unwrap_or(&none)) {
            caps.insert(cap.clone(), rev);
        }
    }
    Ok(caps)
}

/// 03 §3.3 的升级目标：服务端没有该能力或客户端的最低修订高于服务端的最高修订 → 服务端旧；服务端有、客户端的最高修订
/// 低于服务端的最低修订 → 客户端旧；两类都有 → both。
pub fn upgrade_target(missing: &[Missing]) -> InitializeRejectionUpgradeTarget {
    let min = |revs: &[u64]| revs.iter().min().copied();
    let max = |revs: &[u64]| revs.iter().max().copied();
    let server_old = missing.iter().any(|m| {
        m.mine.is_empty() || matches!((min(&m.theirs), max(&m.mine)), (Some(t), Some(s)) if t > s)
    });
    let client_old = missing.iter().any(|m| {
        !m.mine.is_empty()
            && (m.theirs.is_empty()
                || matches!((max(&m.theirs), min(&m.mine)), (Some(t), Some(s)) if t < s))
    });
    match (server_old, client_old) {
        (true, true) => InitializeRejectionUpgradeTarget::Both,
        (true, false) => InitializeRejectionUpgradeTarget::Server,
        _ => InitializeRejectionUpgradeTarget::Client,
    }
}

#[cfg(test)]
#[path = "caps_tests.rs"]
mod tests;
