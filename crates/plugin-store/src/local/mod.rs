//! 本机存储（plugin-store.md §2.1、§3.2）：Node 的文件布局、存储锁、暂存与提交、市场目录、孤儿目录、Node 死事务的收尾、
//! 私有文件与读侧。
//!
//! 写的形状（§5、§9.2）：锁外 [`LocalStore::prepare_install`] / [`LocalStore::prepare_marketplace`] 把内容取进暂存目录
//! （可取消）；[`LocalStore::lock`] 拿存储锁；锁内 [`Locked`] 的方法提交。调用方（cli 的服务）在持锁时改 `config.json`，
//! 锁序见 §4。

mod fetch;
#[cfg(test)]
#[path = "fixture_tests.rs"]
pub(crate) mod fixture;
pub mod install;
pub mod inventory;
pub mod layout;
mod lock;
pub mod market;
mod node_tx;
mod orphans;
mod secrets_file;
mod stage;
pub mod validate;
mod zip;

use std::sync::Arc;

use rust_cli_net::Egress;
use tokio::sync::Semaphore;

pub use layout::Layout;
pub use lock::Locked;

/// 写入的串行化（plugin-store.md §4，A16）：进程内按到达顺序的门。同一个存储根的全部 [`LocalStore`] 共用一个。
#[derive(Debug, Clone)]
pub struct Gate(Arc<Semaphore>);

impl Gate {
    pub fn new() -> Self {
        Self(Arc::new(Semaphore::new(1)))
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

/// 一个插件存储根。
pub struct LocalStore {
    layout: Layout,
    egress: Arc<Egress>,
    gate: Gate,
}

impl LocalStore {
    pub fn new(layout: Layout, egress: Arc<Egress>, gate: Gate) -> Self {
        Self {
            layout,
            egress,
            gate,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }
}
