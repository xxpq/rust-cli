//! 插件管理的 topic 与读（docs/specs/rust-cli/protocol-v5.md §72.2；docs/zcode-protocol-v5/24-plugins.md §2、§9）：用户级
//! `catalog/plugins` 与工作区级 `ws/<key>/catalog/plugins`。第一次订阅时在任务里读一次，读完登记、回 `subscribed`（同会话列表的
//! 物化，§14.4）；之后只在写操作完成与 `plugins.rescan` 时重读（没有文件监听）。
//!
//! 每次读带一个递增的代号，每个 topic 只应用比它已应用的更新的那次读：写之后的重读在提交之后才开始，代号更大；在它之前开始、
//! 晚到的读被丢掉（24 §9 第 4 行）。不用计时器、不用锁。命令与写操作在 `plugin_ops.rs`。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use crate::commands::field_path;
use crate::errors::{Fault, protocol_error};
use crate::generated::catalog_plugins_v1::{PluginDiagnostic, QueryPluginsDescribeParams};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandResultAcceptedEffectsItem, Cursor, QueryEnvelope, QueryResult, QueryResultOk, Subscribe,
};
use crate::plugin_admin::{
    PluginAdmin, PluginApplied, PluginInventory, PluginRefusal, WorkspacePlugins,
};
use crate::plugin_ops::{Operations, Settled, refusal, schemas};
use crate::publisher::{Batch, Fields, Publisher};
use crate::server::{Actor, Event};
use crate::topics::{PLUGINS_TOPIC, WORKSPACE_PLUGINS, workspace_topic};

/// 集合与 state 的键（24 §2；同 `@zcode/protocol` 的 `PLUGIN_COLLECTIONS`）。
pub(crate) const PLUGINS: &str = "plugins";
const MARKETPLACES: &str = "marketplaces";
const CATALOG: &str = "catalog";
pub(crate) const OPERATIONS: &str = "operations";
const DIAGNOSTICS: &str = "diagnostics";
/// 唯一的查询（24 §5）。
pub(crate) const PLUGINS_DESCRIBE: &str = "plugins.describe";

/// 等 topic 登记的订阅。
struct Waiter {
    id: Value,
    sub: String,
    cursor: Option<Cursor>,
}

/// 一个 topic 的读：已应用的代号（0 = 还没有）；还没登记时等它的订阅。
#[derive(Default)]
struct Scope {
    applied: u64,
    waiting: Vec<Waiter>,
}

/// 一次读做完之后要做的。
pub(crate) enum Then {
    Nothing,
    /// `plugins.rescan` 的 ACK（JSON-RPC 的 id）。
    Rescan(Value),
    /// 一条写操作进终态、回 ACK（plugin_ops.rs）。
    Settle(Settled),
}

/// 任务发回 actor 的（`Event::Plugins`）。
pub(crate) enum PluginEvent {
    Read {
        generation: u64,
        inventory: Option<PluginInventory>,
        workspaces: Vec<(String, WorkspacePlugins)>,
        then: Then,
    },
    /// 端口做完了。带着做它的端口：写之后要用它重读（§72.2）。
    Done {
        command_id: String,
        result: Result<PluginApplied, PluginRefusal>,
        admin: Arc<dyn PluginAdmin>,
    },
}

/// actor 里的插件管理（`Actor.plugins`，§72.2）。
#[derive(Default)]
pub(crate) struct Plugins {
    /// 入口交的端口；没有就不声明 `catalog.plugins`、不认这些 topic 与命令。
    pub(crate) admin: Option<Arc<dyn PluginAdmin>>,
    next_generation: u64,
    user: Scope,
    /// 订过的工作区（已登记或正在登记）。
    workspaces: BTreeMap<String, Scope>,
    /// 在途的读（`shutdown` 的回复等它们，§72.4）。
    reads: usize,
    pub(crate) ops: Operations,
}

impl Plugins {
    pub(crate) fn new(admin: Option<Arc<dyn PluginAdmin>>) -> Self {
        Self {
            admin,
            ..Self::default()
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.reads > 0 || self.ops.busy()
    }
}

/// 生成的类型 → 发布用的字段（生成的结构体总是序列化成对象）。
pub(crate) fn fields(value: &impl Serialize) -> Fields {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => Fields::new(),
    }
}

fn entities<T: Serialize>(items: &BTreeMap<String, T>) -> BTreeMap<String, Fields> {
    items
        .iter()
        .map(|(id, item)| (id.clone(), fields(item)))
        .collect()
}

/// 一次读的结果换进 topic：每个集合整份 put（没变的项 Publisher 不发）、删掉不在的 id、换 `diagnostics`。一批一个 seq。
fn replace(
    publisher: &mut Publisher,
    collections: Vec<(&str, BTreeMap<String, Fields>)>,
    diagnostics: &[PluginDiagnostic],
) -> bool {
    let mut batch = Batch::default();
    batch.state(
        DIAGNOSTICS,
        serde_json::to_value(diagnostics).unwrap_or(Value::Null),
    );
    for (col, items) in collections {
        let gone: Vec<String> = publisher
            .ids(col)
            .filter(|id| !items.contains_key(*id))
            .map(str::to_owned)
            .collect();
        for id in gone {
            batch.remove(col, &id);
        }
        for (id, item) in items {
            batch.put(col, &id, item);
        }
    }
    publisher.publish(batch)
}

impl Actor {
    /// 订 `catalog/plugins`（`key` 为 None）或 `ws/<key>/catalog/plugins`：已登记就挂上；否则记下订阅、第一次就起读（§72.2）。
    /// 没有端口时分派处就回 `precondition.notFound`，到不了这里。
    pub(crate) fn subscribe_plugins(
        &mut self,
        admin: Arc<dyn PluginAdmin>,
        id: &Value,
        subscribe: Subscribe,
        key: Option<String>,
    ) {
        let topic = key.as_deref().map_or_else(
            || PLUGINS_TOPIC.to_owned(),
            |key| workspace_topic(key, WORKSPACE_PLUGINS),
        );
        if self.topics.contains_key(&topic) {
            self.outbox
                .subscribe(id.clone(), &subscribe.sub, &topic, subscribe.cursor);
            return;
        }
        self.outbox
            .hold(&subscribe.sub, &topic, subscribe.cursor.clone());
        let scope = match &key {
            Some(key) => self.plugins.workspaces.entry(key.clone()).or_default(),
            None => &mut self.plugins.user,
        };
        let first = scope.waiting.is_empty();
        scope.waiting.push(Waiter {
            id: id.clone(),
            sub: subscribe.sub,
            cursor: subscribe.cursor,
        });
        // 同一个 topic 正在登记时只读一次
        if first {
            self.read_plugins(
                admin,
                key.is_none(),
                key.into_iter().collect(),
                Then::Nothing,
            );
        }
    }

    /// 重读已登记与正在登记的 topic（写操作完成、`plugins.rescan`）：读完做 `then`。
    pub(crate) fn reread_plugins(&mut self, admin: Arc<dyn PluginAdmin>, then: Then) {
        let user = self.topics.contains_key(PLUGINS_TOPIC) || !self.plugins.user.waiting.is_empty();
        let keys = self.plugins.workspaces.keys().cloned().collect();
        self.read_plugins(admin, user, keys, then);
    }

    /// 在任务里读（actor 不等，§4.1），带一个新的代号；读完发 [`PluginEvent::Read`] 回来。
    fn read_plugins(
        &mut self,
        admin: Arc<dyn PluginAdmin>,
        user: bool,
        keys: Vec<String>,
        then: Then,
    ) {
        self.plugins.next_generation += 1;
        self.plugins.reads += 1;
        let (generation, events) = (self.plugins.next_generation, self.events.clone());
        drop(self.spawner.spawn("protocol-v5-plugins-read", async move {
            let inventory = if user {
                Some(admin.inventory().await)
            } else {
                None
            };
            let mut workspaces = Vec::with_capacity(keys.len());
            for key in keys {
                let facts = admin.workspace(&key).await;
                workspaces.push((key, facts));
            }
            let _ = events.send(Event::Plugins(Box::new(PluginEvent::Read {
                generation,
                inventory,
                workspaces,
                then,
            })));
        }));
    }

    pub(crate) fn plugins_event(&mut self, event: PluginEvent) {
        match event {
            PluginEvent::Read {
                generation,
                inventory,
                workspaces,
                then,
            } => {
                self.plugins.reads -= 1;
                if let Some(inventory) = inventory {
                    self.apply_inventory(generation, inventory);
                }
                for (key, facts) in workspaces {
                    self.apply_workspace(generation, &key, facts);
                }
                match then {
                    Then::Nothing => {}
                    Then::Rescan(reply) => self.rescanned(reply),
                    Then::Settle(settled) => self.settle_operation(settled),
                }
            }
            PluginEvent::Done {
                command_id,
                result,
                admin,
            } => {
                // 写之后重读，读完再进终态：ACK 在新的事实发出之后（§72.2）
                self.reread_plugins(admin, Then::Settle(Settled { command_id, result }));
            }
        }
        self.finish_shutdown();
    }

    fn apply_inventory(&mut self, generation: u64, inventory: PluginInventory) {
        if generation <= self.plugins.user.applied {
            return;
        }
        self.plugins.user.applied = generation;
        let collections = vec![
            (PLUGINS, entities(&inventory.plugins)),
            (MARKETPLACES, entities(&inventory.marketplaces)),
            (CATALOG, entities(&inventory.catalog)),
        ];
        if let Some(publisher) = self.topics.get_mut(PLUGINS_TOPIC) {
            if replace(publisher, collections, &inventory.diagnostics) {
                self.outbox.mark_topic(PLUGINS_TOPIC);
            }
            return;
        }
        // 第一次：操作表也放进去（之后它随操作变，plugin_ops.rs）
        let mut collections = collections;
        collections.push((OPERATIONS, self.plugins.ops.entities()));
        let waiting = std::mem::take(&mut self.plugins.user.waiting);
        self.register_plugins(
            PLUGINS_TOPIC.to_owned(),
            collections,
            &inventory.diagnostics,
            waiting,
        );
    }

    fn apply_workspace(&mut self, generation: u64, key: &str, facts: WorkspacePlugins) {
        // 读的工作区来自这张表（订过的才读，表里的项不删）
        let scope = self.plugins.workspaces.entry(key.to_owned()).or_default();
        if generation <= scope.applied {
            return;
        }
        scope.applied = generation;
        let waiting = std::mem::take(&mut scope.waiting);
        let topic = workspace_topic(key, WORKSPACE_PLUGINS);
        let collections = vec![(PLUGINS, entities(&facts.plugins))];
        if let Some(publisher) = self.topics.get_mut(&topic) {
            if replace(publisher, collections, &facts.diagnostics) {
                self.outbox.mark_topic(&topic);
            }
            return;
        }
        self.register_plugins(topic, collections, &facts.diagnostics, waiting);
    }

    /// 物化完再登记（01 §5.2）：首帧之后排等着的 `subscribed`。
    fn register_plugins(
        &mut self,
        topic: String,
        collections: Vec<(&str, BTreeMap<String, Fields>)>,
        diagnostics: &[PluginDiagnostic],
        waiting: Vec<Waiter>,
    ) {
        let mut publisher = Publisher::new(topic.clone(), self.epoch.clone());
        replace(&mut publisher, collections, diagnostics);
        self.topics.insert(topic.clone(), publisher);
        self.outbox.mark_topic(&topic);
        for Waiter { id, sub, cursor } in waiting {
            self.outbox.subscribed(id, &sub, cursor);
        }
    }

    /// ACK 的 effects：插件管理的每个已登记 topic 的当前位置（24 §4）。
    pub(crate) fn plugin_effects(&self) -> Vec<CommandResultAcceptedEffectsItem> {
        std::iter::once(PLUGINS_TOPIC.to_owned())
            .chain(
                self.plugins
                    .workspaces
                    .keys()
                    .map(|key| workspace_topic(key, WORKSPACE_PLUGINS)),
            )
            .filter_map(|topic| {
                let cursor = self.topics.get(&topic)?.cursor();
                Some(CommandResultAcceptedEffectsItem {
                    topic,
                    epoch: cursor.epoch,
                    seq: cursor.seq,
                })
            })
            .collect()
    }

    /// `plugins.describe`（24 §5）：在任务里问端口，回复走 [`Event::Queried`]（同 `sessions.facts`）。
    pub(crate) fn plugins_describe(
        &mut self,
        admin: Arc<dyn PluginAdmin>,
        reply: &Value,
        query: QueryEnvelope,
    ) {
        self.answer_in_task(
            "protocol-v5-plugins-describe",
            reply,
            describe(admin, query),
        );
    }
}

fn fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

/// 参数照 schema 校验（查询参数宽松，11 §11 第 3 项），再问端口。
async fn describe(admin: Arc<dyn PluginAdmin>, query: QueryEnvelope) -> Result<Value, Fault> {
    let params = query
        .params
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if let Some(schema) = &schemas().describe
        && let Err(error) = schema.validate(&params, "")
    {
        let (field, message) = field_path(&error);
        return Err(fault(
            codes::PRECONDITION_INVALID_PAYLOAD,
            format!("{field}: {message}"),
        ));
    }
    let params: QueryPluginsDescribeParams = serde_json::from_value(params)
        .map_err(|error| fault(codes::PRECONDITION_INVALID_PAYLOAD, error.to_string()))?;
    let described = admin.describe(&params.plugin_id).await.map_err(|refused| {
        let (code, _, message) = refusal(&refused);
        fault(code, message)
    })?;
    let data = serde_json::to_value(described).unwrap_or(Value::Null);
    Ok(
        serde_json::to_value(QueryResult::Ok(QueryResultOk { data, cursor: None }))
            .unwrap_or(Value::Null),
    )
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
