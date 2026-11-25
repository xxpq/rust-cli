//! 会话列表（protocol-v5.md §14；02 §8、§11.2 C；04 §3）：每个工作区一份 `ws/<key>/sessions`，与按 ID 查事实的
//! `sessions.facts`。事实、种子与变化流都来自内核的存储查询（docs/specs/rust-cli/session-facts.md），这里只投影成帧与查询结果。
//!
//! ```text
//! 订阅 ws/<key>/sessions ── topic 已登记 ──────────► 同 §7（帧标记 → 首帧 → subscribed）
//!                        ├─ 有存储查询 ─► 记下订阅；读种子的任务（①订变化流 ②读种子）─ Seeded ─► ③并进排着的变化 ④登记 → 同上
//!                        └─ 还没有 ─────► subscribed{pending}；进程级部件建好时物化，等着的订阅拿快照
//! 之后：变化流的每一项（只要这个工作区的）→ 一批 entity.put（值没变不发）
//! sessions.facts ─► 查询的任务（校验、读库、换算）─ Queried ─► 直接回复
//! ```
//!
//! actor 不等读库（protocol-v5.md §4.1）：读都在任务里，结果是发给 actor 的消息。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::sync::Arc;

use futures_util::stream::{BoxStream, SelectAll};
use futures_util::{FutureExt, StreamExt, future};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_facts::{
    MigrationSource, SessionFacts, SessionOutcome, TitleSource,
};
use rust_cli_kernel_api::store::{StoreError, StoreQueries};
use serde_json::Value;

use crate::commands::field_path;
use crate::consts::SESSIONS_SNAPSHOT_FACTS;
use crate::errors::{Fault, protocol_error, to_rpc};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    Cursor, ProtocolError, QueryEnvelope, QueryResult, QueryResultOk, Subscribe,
};
use crate::generated::session_v1::{
    QuerySessionsFactsParams, QuerySessionsFactsResult, QuerySessionsFactsResultSessionsItem,
    SessionFactFields, SessionFactFieldsConfig, SessionFactFieldsLastOutcome,
    SessionFactFieldsMigrationSource, SessionFactFieldsTitleSource,
};
use crate::publisher::{Fields, Publisher};
use crate::server::{Actor, Event, Workspaces};
use crate::topics::{self, WORKSPACE_SESSIONS, workspace_topic};

/// 按会话 ID 查事实的查询类型（session@1，02 §8）。
pub(crate) const SESSIONS_FACTS: &str = "sessions.facts";
/// 会话列表的集合名（04 §3 的 `facts`）。
const FACTS: &str = "facts";

/// 一个工作区的事实变化（带它的 key）。
type Changes = BoxStream<'static, (String, SessionFacts)>;

/// 读种子的任务读到的（§14.4 的 ①②）：种子与订在它之前的变化流（已按工作区过滤）。③④在 actor 里做。
pub(crate) struct Seed {
    rows: BTreeMap<String, SessionFacts>,
    changes: BoxStream<'static, SessionFacts>,
}

/// 物化中的一个订阅请求：登记之后排它的 `subscribed`，读不出时回错误。
struct Waiter {
    id: Value,
    sub: String,
    cursor: Option<Cursor>,
}

/// actor 里的会话列表状态。
#[derive(Default)]
pub(crate) struct Feeds {
    /// 进程级部件建好之前就有人订阅的工作区（key）：建好时物化。
    waiting: BTreeSet<String>,
    /// 正在物化的工作区（读种子的任务还没回来）→ 等首帧的订阅请求。同一个工作区只读一次（§14.4）。
    loading: BTreeMap<String, Vec<Waiter>>,
    /// 在途的、在任务里答的查询（`sessions.facts` §14.6；插件引用目录 §59.1）。
    queries: usize,
    /// 已物化的每个工作区一条变化流（已按工作区过滤），合在一起交给 actor 的 select。
    pub(crate) changes: SelectAll<Changes>,
}

impl Feeds {
    /// 还有读库的任务没回来：`shutdown` 的回复等它们（§5.2）。
    pub(crate) fn busy(&self) -> bool {
        !self.loading.is_empty() || self.queries > 0
    }
}

/// 毫秒数（内核的 `u64`）→ 线上的数字（生成的类型是 `f64`）。毫秒时间远小于 2^53，换算不丢精度。
#[expect(
    clippy::cast_precision_loss,
    reason = "毫秒时间远小于 2^53，f64 能精确表示"
)]
fn millis(ms: u64) -> f64 {
    ms as f64
}

/// 内核的会话事实 → FactRow 的字段（protocol-v5.md §14.2 的表）。`summary` 不带：内核没有这些事实（§13 第 10 项）。
pub(crate) fn fields(facts: &SessionFacts) -> SessionFactFields {
    let title_source = match facts.title_source {
        TitleSource::Default => SessionFactFieldsTitleSource::Default,
        TitleSource::FirstInput => SessionFactFieldsTitleSource::FirstInput,
        TitleSource::Generated => SessionFactFieldsTitleSource::Generated,
        TitleSource::Custom => SessionFactFieldsTitleSource::Custom,
    };
    let last_outcome = facts.last_outcome.map(|outcome| match outcome {
        SessionOutcome::Completed => SessionFactFieldsLastOutcome::Success,
        SessionOutcome::Interrupted => SessionFactFieldsLastOutcome::Cancelled,
        SessionOutcome::Error => SessionFactFieldsLastOutcome::Error,
    });
    let selection = facts.selection.as_ref();
    SessionFactFields {
        title: facts.title.clone(),
        title_source,
        created_at: millis(facts.created_at_ms),
        updated_at: millis(facts.updated_at_ms),
        last_outcome,
        deleted_at: facts.deleted_at_ms.map(millis),
        parent_session_id: facts
            .parent
            .as_ref()
            .map(|parent| parent.as_str().to_owned()),
        first_input_at: facts.first_input_at_ms.map(millis),
        migration_source: facts.migration.map(|MigrationSource::ImportedHistory| {
            SessionFactFieldsMigrationSource::ImportedHistory
        }),
        // 模式用 App 的模式名（Node 名，同 v4 投影的 mode_name，§13 第 11 项）；没有记下的为空字符串（契约的缺省）
        config: SessionFactFieldsConfig {
            model: selection.map(|s| s.model.clone()).unwrap_or_default(),
            mode: facts
                .execution
                .map(|execution| execution.mode.node_name().to_owned())
                .unwrap_or_default(),
            thought: selection
                .and_then(|s| s.reasoning.clone())
                .unwrap_or_default(),
        },
        summary: None,
    }
}

/// 一行 FactRow：id 与字段（发布用的形状）。
fn row(facts: &SessionFacts) -> (String, Fields) {
    let fields = match serde_json::to_value(fields(facts)) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        // 生成的结构体总是序列化成对象（同本 crate 别处序列化的 `unwrap_or`）：这一支走不到
        _ => Fields::new(),
    };
    (facts.id.as_str().to_owned(), fields)
}

/// 存储读不出：源暂时不可用（可重试，errors.ts 的 `precondition.storeUnavailable`）。
fn store_unavailable(error: impl Display) -> ProtocolError {
    protocol_error(codes::PRECONDITION_STORE_UNAVAILABLE, error.to_string())
}

fn fault(code: &str, message: impl Into<String>) -> Fault {
    Box::new(protocol_error(code, message))
}

/// 进程级部件里会话列表要的两样：工作区端口与存储查询。
pub(crate) type Sources = (Arc<dyn Workspaces>, Arc<dyn StoreQueries>);

impl Actor {
    /// 进程级部件建好之后才有，两样同时出现（`process_ready`）。拿走的是 Arc：读库时 actor 不借出自己（它不是 Sync）。
    pub(crate) fn sources(&self) -> Option<Sources> {
        Some((
            self.process.workspaces.clone()?,
            self.process.queries.clone()?,
        ))
    }

    /// 订阅 `ws/<key>/sessions`（§14.4）。
    pub(crate) fn subscribe_sessions(&mut self, id: &Value, subscribe: Subscribe, key: &str) {
        let topic = workspace_topic(key, WORKSPACE_SESSIONS);
        if self.topics.contains_key(&topic) {
            self.outbox
                .subscribe(id.clone(), &subscribe.sub, &topic, subscribe.cursor);
            return;
        }
        let Some(sources) = self.sources() else {
            // 进程级部件还没建好：源暂时不可用（01 §5.2），建好时物化，这个订阅拿快照
            self.feeds.waiting.insert(key.to_owned());
            self.outbox
                .subscribe_pending(id.clone(), &subscribe.sub, &topic, subscribe.cursor);
            return;
        };
        // 读种子在任务里（§4.1）：订阅先记下，登记时拿首帧，回复排在首帧之后
        self.outbox
            .hold(&subscribe.sub, &topic, subscribe.cursor.clone());
        self.load(key, &sources).push(Waiter {
            id: id.clone(),
            sub: subscribe.sub,
            cursor: subscribe.cursor,
        });
    }

    /// 进程级部件建好：物化之前就有人订阅的会话列表，每个工作区一个读种子的任务。
    pub(crate) fn materialize_waiting(&mut self, sources: &Sources) {
        for key in std::mem::take(&mut self.feeds.waiting) {
            self.load(&key, sources);
        }
    }

    /// 这个工作区正在物化时等它的请求；还没有在物化就起读种子的任务（①②），读完发 [`Event::Seeded`] 回来。同一个工作区只读一次。
    ///
    /// 缺陷修复（ACTOR-IO）：以前在 actor 里 `await` 种子的读。会话库只有一条连接、外面一把互斥锁，别的进程拿着写锁时本进程的写在锁里
    /// 等 busy 超时（5 s），读排在它后面；actor 是整条链路唯一的 owner，这段时间里全部工作区的帧、命令的 ACK 与 `ping` 一起停住。
    /// 改成任务，actor 只在结果回来时登记。
    fn load(&mut self, key: &str, (workspaces, queries): &Sources) -> &mut Vec<Waiter> {
        self.feeds.loading.entry(key.to_owned()).or_insert_with(|| {
            let (workspaces, queries) = (workspaces.clone(), queries.clone());
            let (events, owned) = (self.events.clone(), key.to_owned());
            drop(self.spawner.spawn("protocol-v5-sessions-seed", async move {
                let result = seed(&owned, &*workspaces, &*queries).await;
                let _ = events.send(Event::Seeded { key: owned, result });
            }));
            Vec::new()
        })
    }

    /// 读种子的任务回来了：③④，等着的请求排 `subscribed`（首帧之后）。读不出时等着的请求回错误，进程级部件建好之前就在等的订阅收
    /// `subscription.failed`；都不登记，下一次订阅重新读。
    pub(crate) fn seeded(&mut self, key: &str, result: Result<Seed, StoreError>) {
        let waiters = self.feeds.loading.remove(key).unwrap_or_default();
        match result {
            Ok(seed) => {
                self.register(key, seed);
                for Waiter { id, sub, cursor } in waiters {
                    self.outbox.subscribed(id, &sub, cursor);
                }
            }
            Err(error) => {
                tracing::warn!(event = "protocol.sessions.seed_failed", workspace = key, error = %error, "reading the session list seed failed");
                let topic = workspace_topic(key, WORKSPACE_SESSIONS);
                let error = store_unavailable(error);
                for Waiter { id, sub, .. } in waiters {
                    self.outbox.refuse(id, &sub, &topic, to_rpc(&error));
                }
                self.outbox.fail_topic(&topic, &error);
            }
        }
        self.finish_shutdown();
    }

    /// ③④ 并进排着的变化、登记（物化完再登记，01 §5.2）：这个 topic 的订阅挂上帧标记；之后的变化进 actor 的 select。
    fn register(
        &mut self,
        key: &str,
        Seed {
            mut rows,
            mut changes,
        }: Seed,
    ) {
        // ③ 读种子之前的提交在连接锁里先发出，这时都已在通道里（种子里也有）。并进种子再登记：不并的话订阅者先拿到新值，又收到更早的
        // 那次提交（旧值），列表会短暂退回旧标题。并进去之后，发出去的每个状态都按提交的顺序前进。在 actor 里并：投影已经应用到的内核
        // 事件，内核在发它之前已经落库、发进了通道，所以首帧的事实不比下面的 `live` 旧（§14.4）
        while let Some(Some(facts)) = changes.next().now_or_never() {
            rows.insert(facts.id.as_str().to_owned(), facts);
        }
        let topic = workspace_topic(key, WORKSPACE_SESSIONS);
        let mut publisher = Publisher::new(topic.clone(), self.epoch.clone());
        publisher.put(FACTS, rows.values().map(row).collect());
        // 首帧带这个工作区里正在跑的会话（`live`，§14.1）：来自会话的投影，之后随投影变
        publisher.put(crate::session::LIVE, self.live_rows(key));
        self.topics.insert(topic.clone(), publisher);
        let key = key.to_owned();
        self.feeds
            .changes
            .push(changes.map(move |facts| (key.clone(), facts)).boxed());
        self.outbox.mark_topic(&topic);
    }

    /// `sessions.facts` 交给任务（§14.6）：校验、读库、换算都在任务里，做完发 [`Event::Queried`] 回来。
    ///
    /// 缺陷修复（ACTOR-IO）：以前在 actor 里 `await` 这次读，原因同 [`Self::load`]。
    pub(crate) fn query_facts(&mut self, reply: &Value, query: QueryEnvelope) {
        let facts = sessions_facts(self.sources(), query);
        self.answer_in_task("protocol-v5-sessions-facts", reply, facts);
    }

    /// 在任务里答一个查询（§4.1：读库、组装工作区不在 actor 里做）：回复等 [`Event::Queried`]，`shutdown` 的回复等它（在途计数）。
    pub(crate) fn answer_in_task(
        &mut self,
        name: &'static str,
        reply: &Value,
        answer: impl Future<Output = Result<Value, Fault>> + Send + 'static,
    ) {
        let (events, reply) = (self.events.clone(), reply.clone());
        self.feeds.queries += 1;
        drop(self.spawner.spawn(name, async move {
            let result = answer.await;
            let _ = events.send(Event::Queried { reply, result });
        }));
    }

    /// 查询的任务回来了：直接写出回复（不排进出站队列：查询结果与帧不保证先后，不等被信用挡住的帧，§14.6）。
    pub(crate) async fn queried(&mut self, reply: Value, result: Result<Value, Fault>) {
        self.feeds.queries -= 1;
        self.peer
            .respond(&reply, &result.map_err(|error| to_rpc(&error)))
            .await;
        self.finish_shutdown();
    }

    /// 把会话列表的变化流里已经到了的变化都发出去（不等）：回执与活性之前用，事实先到（§15.4.2、§14.1）。
    pub(crate) fn drain_fact_changes(&mut self) {
        while let Some(Some((key, facts))) = self.feeds.changes.next().now_or_never() {
            self.fact_changed(&key, &facts);
        }
    }

    /// 一条事实变化（已是这个工作区的）：一批 `entity.put`；值没变时不发、不占 seq。
    pub(crate) fn fact_changed(&mut self, key: &str, facts: &SessionFacts) {
        let topic = workspace_topic(key, WORKSPACE_SESSIONS);
        if let Some(publisher) = self.topics.get_mut(&topic)
            && publisher.put(FACTS, vec![row(facts)])
        {
            self.outbox.mark_topic(&topic);
        }
    }
}

/// `sessions.facts{sessionIds}`（§14.6）：按调用方的工作区隔离；每个请求的 id 一项，按请求的顺序。
pub(crate) async fn sessions_facts(
    sources: Option<Sources>,
    query: QueryEnvelope,
) -> Result<Value, Fault> {
    let Some(key) = query.target.workspace_key.as_deref() else {
        return Err(fault(
            codes::PRECONDITION_WORKSPACE_REQUIRED,
            "sessions.facts needs target.workspaceKey",
        ));
    };
    topics::local_workspace(key).map_err(|message| fault(codes::PRECONDITION_INVALID, message))?;
    let params = query.params.unwrap_or(Value::Null);
    // 上限 256（`SESSIONS_FACTS_QUERY_MAX`）只写在契约里，这里照 schema 校验，不另写一份
    if let Some(schema) = crate::session::schemas::query(SESSIONS_FACTS)
        && let Err(error) = schema.validate(&params, "")
    {
        let (field, message) = field_path(&error);
        return Err(fault(
            codes::PRECONDITION_INVALID_PAYLOAD,
            format!("{field}: {message}"),
        ));
    }
    let params: QuerySessionsFactsParams = serde_json::from_value(params)
        .map_err(|error| fault(codes::PRECONDITION_INVALID_PAYLOAD, error.to_string()))?;
    let Some((workspaces, queries)) = sources else {
        return Err(Box::new(store_unavailable(
            "the session store is not ready yet",
        )));
    };
    let workspace = workspaces.workspace(key);
    let ids: Vec<SessionId> = params.session_ids.iter().map(SessionId::new).collect();
    let found = queries
        .session_facts(&ids)
        .await
        .map_err(|error| Box::new(store_unavailable(error)))?;
    // 内核的端口不过滤工作区：只留调用方工作区的，别的工作区的与库里没有的同样是 null（§13 第 9 项）
    let own: HashMap<&str, &SessionFacts> = found
        .iter()
        .filter(|facts| facts.workspace == workspace)
        .map(|facts| (facts.id.as_str(), facts))
        .collect();
    let sessions = params
        .session_ids
        .into_iter()
        .map(|session_id| QuerySessionsFactsResultSessionsItem {
            fact: own.get(session_id.as_str()).map(|facts| fields(facts)),
            session_id,
        })
        .collect();
    let data = serde_json::to_value(QuerySessionsFactsResult { sessions }).unwrap_or(Value::Null);
    Ok(
        serde_json::to_value(QueryResult::Ok(QueryResultOk { data, cursor: None }))
            .unwrap_or(Value::Null),
    )
}

/// 读种子（§14.4 的 ①②，在任务里）：先订这个工作区的变化流，再读种子。
async fn seed(
    key: &str,
    workspaces: &dyn Workspaces,
    queries: &dyn StoreQueries,
) -> Result<Seed, StoreError> {
    let workspace = workspaces.workspace(key);
    let own = workspace.clone();
    // ① 先订阅：②之后的提交一定在通道里（session-facts.md §3.1）
    let changes = queries
        .session_fact_changes()
        .filter(move |facts| future::ready(facts.workspace == own))
        .boxed();
    // ② 再读种子
    let rows = queries
        .recent_session_facts(&workspace, SESSIONS_SNAPSHOT_FACTS)
        .await?
        .into_iter()
        .map(|facts| (facts.id.as_str().to_owned(), facts))
        .collect();
    Ok(Seed { rows, changes })
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
