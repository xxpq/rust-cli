//! 一个 topic 的权威日志（01 §5.1；续传规则 01 §19.2；与 protocol-runtime `store/publisher.ts` 同一规则）：本进程的 epoch、seq、当前状态
//! （state 与集合），以及续传要用的账——每个实体最后一次变化的 seq（`changed`）与出现的 seq（`born`）、只记「谁变了 + 追加的块」的保留日志
//! （按字节预算）、删掉的项的墓碑（按条数）、两条边界（`rewritten`、`lossy`）。拼帧在 [`resume`]（protocol-v5.md §68.3）。
//!
//! 一批可以混合 02 §2 的 op（protocol-v5.md §15.3.6）：`state.set` 按键整体替换，`entity.put` 整项替换，`entity.append` 对字符串字段追加
//! （不改 `v`），`entity.remove` 删一项。值没变的 `state.set` 与 `entity.put` 不发，一批全空不占 seq。已在内存的整行收到 put 时按顶层键比，
//! 只记变了的键（线上是 `entity.patch`，没协商出 `core.patch` 的连接收到当前整行的 put，01 §19.4）。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::consts::{RETAINED_LOG_BYTES, RETAINED_TOMBSTONES};
use crate::generated::core_v1::Cursor;

#[path = "publisher_resume.rs"]
mod resume;

use resume::{Log, Tombstones, Touch};

/// 实体的字段（不含 `id` 与 `v`）。
pub type Fields = BTreeMap<String, Value>;

/// topic 的内容：state 区与集合（集合名 → id → 字段）。`adopt` 用它整份替换。
///
/// `outlined` 是建的时候就只留了骨架的项（冷读只建窗口，protocol-v5.md §71.3）：集合名 → id → `outline` 那几个键的值；它们与 `collections`
/// 里的同一集合合起来才是这个集合的全部项。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Content {
    pub state: BTreeMap<String, Value>,
    pub collections: BTreeMap<String, BTreeMap<String, Fields>>,
    pub outlined: BTreeMap<String, BTreeMap<String, Box<[Value]>>>,
    pub outline: &'static [&'static str],
}

/// 集合里的一项：版本、续传的账与字段。
#[derive(Debug, Clone, PartialEq)]
struct Entity {
    v: u64,
    /// 最后一次被任何 op 碰到的 seq（01 §19.2）。
    changed: u64,
    /// 在这一份 Publisher 里出现的 seq。
    born: u64,
    body: Body,
}

/// 一项的内容：整行，或窗口外只留的骨架（protocol-v5.md §67）。
#[derive(Debug, Clone, PartialEq)]
enum Body {
    Full(Fields),
    /// [`Publisher::thin`] 给的那几个键的值（按键的顺序）：排序、种类与回合够用，内容要时从存储重建。
    Outline(Box<[Value]>),
}

impl Entity {
    fn full(v: u64, seq: u64, fields: Fields) -> Self {
        Self {
            v,
            changed: seq,
            born: seq,
            body: Body::Full(fields),
        }
    }

    fn fields(&self) -> Option<&Fields> {
        match &self.body {
            Body::Full(fields) => Some(fields),
            Body::Outline(_) => None,
        }
    }

    /// 一个字段：整行的直接取，骨架的只有骨架的键。
    fn value<'a>(&'a self, outline: &[&str], key: &str) -> Option<&'a Value> {
        match &self.body {
            Body::Full(fields) => fields.get(key),
            Body::Outline(values) => values.get(outline.iter().position(|k| *k == key)?),
        }
    }

    /// 与这份字段是同一项：整行逐字段比；骨架比骨架的键（骨架只给不再变的项，§67.0 第 3 条）。
    fn matches(&self, outline: &[&str], fields: &Fields) -> bool {
        match &self.body {
            Body::Full(current) => current == fields,
            Body::Outline(values) => *values == outlined(outline, fields),
        }
    }

    /// 与这份骨架是同一项：比骨架的键（整行的取出这几个键）。
    fn matches_outline(&self, outline: &[&str], values: &[Value]) -> bool {
        match &self.body {
            Body::Full(current) => &*outlined(outline, current) == values,
            Body::Outline(current) => &**current == values,
        }
    }
}

/// 一项的骨架：`outline` 里每个键的值（没有的键是 null）。
fn outlined(outline: &[&str], fields: &Fields) -> Box<[Value]> {
    outline
        .iter()
        .map(|key| fields.get(*key).cloned().unwrap_or(Value::Null))
        .collect()
}

/// 一批 op 的构造（发布时一批一个 seq，G3）。
#[derive(Debug, Default)]
pub struct Batch {
    ops: Vec<Op>,
}

#[derive(Debug)]
enum Op {
    State(String, Value),
    Put(String, String, Fields),
    Append {
        col: String,
        id: String,
        path: String,
        chunk: String,
    },
    Remove(String, String),
}

impl Batch {
    pub fn state(&mut self, key: &str, value: Value) {
        self.ops.push(Op::State(key.to_owned(), value));
    }

    pub fn put(&mut self, col: &str, id: &str, fields: Fields) {
        self.ops
            .push(Op::Put(col.to_owned(), id.to_owned(), fields));
    }

    pub fn append(&mut self, col: &str, id: &str, path: &str, chunk: &str) {
        self.ops.push(Op::Append {
            col: col.to_owned(),
            id: id.to_owned(),
            path: path.to_owned(),
            chunk: chunk.to_owned(),
        });
    }

    pub fn remove(&mut self, col: &str, id: &str) {
        self.ops.push(Op::Remove(col.to_owned(), id.to_owned()));
    }
}

/// 快照只带一个集合的尾部（02 §2.3 的会话行窗口）：按 `(field, id)` 排序取最后 `limit` 项，累计的行字节不超过 `max_bytes`（至少一项）；
/// 给了 `opens` 时再从取出的项里的第一个开头起（protocol-v5.md §71.1）。
#[derive(Debug, Clone)]
struct Window {
    col: String,
    field: String,
    limit: usize,
    max_bytes: usize,
    opens: Option<Opens>,
}

/// 一项能不能当快照尾窗（与一页）的第一项（protocol-v5.md §71.1）：读快照里的写法（扁平的 `{id, v, ...字段}`，骨架只有骨架的键）。
pub type Opens = fn(&Value) -> bool;

/// 从 `items` 里的第一个开头起（protocol-v5.md §71.1）：丢掉它前面的项，返回丢了没有；没有开头时不动（一段比窗口还长，照旧按行数）。
pub fn start_at_opening(items: &mut Vec<Value>, opens: Opens) -> bool {
    match items.iter().position(opens) {
        Some(first) if first > 0 => {
            items.drain(..first);
            true
        }
        _ => false,
    }
}

/// 快照里实体的 id 与版本的键（core@1 的实体形状 `{id, v, ...}`）。
pub(crate) const ID_KEY: &str = "id";
pub(crate) const VERSION_KEY: &str = "v";

/// 一个集合按 `(field, id)` 的全序：快照的尾部窗口与分页（[`Publisher::page`]）共用这一个排序，两者切出的行才一致。
fn ordered<'a>(
    entities: &'a BTreeMap<String, Entity>,
    field: &str,
    outline: &[&str],
) -> BTreeSet<(u64, &'a String)> {
    entities
        .iter()
        .map(|(id, entity)| (sort_key(entity, field, outline), id))
        .collect()
}

fn sort_key(entity: &Entity, field: &str, outline: &[&str]) -> u64 {
    entity
        .value(outline, field)
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

/// 快照与分页里的一项：扁平的 `{id, v, ...字段}`（同 TopicStore 的实体）；骨架只有骨架的键。
fn flat(id: &str, entity: &Entity, outline: &[&str]) -> Value {
    let mut item = serde_json::Map::new();
    item.insert(ID_KEY.to_owned(), Value::from(id));
    item.insert(VERSION_KEY.to_owned(), Value::from(entity.v));
    match &entity.body {
        Body::Full(fields) => item.extend(fields.clone()),
        Body::Outline(values) => item.extend(
            outline
                .iter()
                .zip(values.iter())
                .map(|(key, value)| ((*key).to_owned(), value.clone())),
        ),
    }
    Value::Object(item)
}

/// 一项在快照与分页里的 JSON 字节数（窗口的字节预算按它累计，01 §19.5）。
fn json_bytes(item: &Value) -> usize {
    serde_json::to_vec(item).map_or(0, |bytes| bytes.len())
}

/// 一个集合里的一页（`rows.range`，protocol-v5.md §40.2）：按全序升序；`more` = 再往前还有。`outlined` 是这一页里只有骨架的项的 id
/// （按页里的顺序）：它们的内容要从存储重建（§67.0 第 4 条）。
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub items: Vec<Value>,
    pub more: bool,
    pub outlined: Vec<String>,
}

pub struct Publisher {
    topic: String,
    epoch: String,
    seq: u64,
    state: BTreeMap<String, Value>,
    /// state 每个键最后一次变化的 seq（01 §19.2）。
    state_changed: BTreeMap<String, u64>,
    /// 集合名 → id → 项（id 有序：快照里的集合按 id 排序，同 TopicStore 的规范化快照）。
    collections: BTreeMap<String, BTreeMap<String, Entity>>,
    /// 保留日志：每批谁变了、追加的块（不存整行，01 §19.2）。
    log: Log,
    tombstones: Tombstones,
    window: Option<Window>,
    /// 窗口外的项换成骨架时留的键（[`Self::thin`]；没换过时是空的）。
    outline: &'static [&'static str],
    /// 最后一次整份换内容（同一 epoch 的新快照，[`Self::adopt`]）的 seq：查询的 `at` 早于它就过期（02 §2.3）；早于它的 cursor 只能拿快照。
    rewritten: u64,
    /// 当前内容复原不了的最高 seq（丢掉的墓碑、换成骨架的行，01 §19.2）：落后到日志之外又早于它的 cursor 拿快照。
    lossy: u64,
}

impl Publisher {
    pub fn new(topic: impl Into<String>, epoch: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            epoch: epoch.into(),
            seq: 0,
            state: BTreeMap::new(),
            state_changed: BTreeMap::new(),
            collections: BTreeMap::new(),
            log: Log::new(RETAINED_LOG_BYTES),
            tombstones: Tombstones::new(RETAINED_TOMBSTONES),
            window: None,
            outline: &[],
            rewritten: 0,
            lossy: 0,
        }
    }

    /// 快照里集合 `col` 只带按 `(field, id)` 排序的尾部：至多 `limit` 项、累计的行字节至多 `max_bytes`（02 §2.3、01 §19.5）。
    /// 增量不受它限制。
    pub fn with_window(mut self, col: &str, field: &str, limit: usize, max_bytes: usize) -> Self {
        self.window = Some(Window {
            col: col.to_owned(),
            field: field.to_owned(),
            limit,
            max_bytes,
            opens: None,
        });
        self
    }

    /// 快照的尾窗从取出的项里的第一个开头起（protocol-v5.md §71.1）。要先有窗口（[`Self::with_window`]）。
    pub fn with_openings(mut self, opens: Opens) -> Self {
        if let Some(window) = self.window.as_mut() {
            window.opens = Some(opens);
        }
        self
    }

    #[cfg(test)]
    fn with_retention(mut self, log_bytes: usize, tombstones: usize) -> Self {
        self.log = Log::new(log_bytes);
        self.tombstones = Tombstones::new(tombstones);
        self
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    pub fn cursor(&self) -> Cursor {
        Cursor {
            epoch: self.epoch.clone(),
            seq: self.seq,
        }
    }

    /// 按 `at` 读的结果还与 client 手里的内容同序（02 §2.3 的 `at` 没有过期）：同一 epoch，且不早于最后一次新快照。之后只有新的批
    /// （新行的序号更大、追加不动序号）不算过期。
    pub fn current(&self, at: &Cursor) -> bool {
        at.epoch == self.epoch && at.seq >= self.rewritten
    }

    /// 集合 `col` 按 `(field, id)` 的全序里排在 `before` 之前（None：全部）的最后 `limit` 项（`rows.range`，protocol-v5.md §40.2）。
    /// `before` 不在集合里时为 None。
    pub fn page(&self, col: &str, field: &str, before: Option<&str>, limit: usize) -> Option<Page> {
        let empty = BTreeMap::new();
        let entities = self.collections.get(col).unwrap_or(&empty);
        let ordered = ordered(entities, field, self.outline);
        let earlier: Vec<&String> = match before {
            None => ordered.iter().map(|(_, id)| *id).collect(),
            Some(id) => {
                let (id, entity) = entities.get_key_value(id)?;
                ordered
                    .range(..(sort_key(entity, field, self.outline), id))
                    .map(|(_, id)| *id)
                    .collect()
            }
        };
        let skip = earlier.len().saturating_sub(limit);
        let mut outlined = Vec::new();
        let items = earlier[skip..]
            .iter()
            .filter_map(|id| entities.get_key_value(*id))
            .map(|(id, entity)| {
                if entity.fields().is_none() {
                    outlined.push(id.clone());
                }
                flat(id, entity, self.outline)
            })
            .collect();
        Some(Page {
            items,
            more: skip > 0,
            outlined,
        })
    }

    #[cfg(test)]
    pub fn state(&self) -> &BTreeMap<String, Value> {
        &self.state
    }

    /// 一项的整行；没有、或只剩骨架时为 None（要内容的地方只认整行，protocol-v5.md §67.0 第 5 条）。
    pub fn entity(&self, col: &str, id: &str) -> Option<&Fields> {
        self.collections.get(col)?.get(id)?.fields()
    }

    /// 一项的一个字段：整行与骨架都给（骨架只有骨架的键）。
    pub fn field(&self, col: &str, id: &str, key: &str) -> Option<&Value> {
        self.collections.get(col)?.get(id)?.value(self.outline, key)
    }

    /// 一个集合里的全部 id（按 id 的顺序；没有这个集合时为空）。
    pub fn ids(&self, col: &str) -> impl Iterator<Item = &str> {
        self.collections
            .get(col)
            .into_iter()
            .flat_map(|items| items.keys().map(String::as_str))
    }

    /// 一个集合里的全部整行（按 id 的顺序；没有这个集合时为空；骨架不给）。
    pub fn entities(&self, col: &str) -> impl Iterator<Item = &Fields> {
        self.collections
            .get(col)
            .into_iter()
            .flat_map(|items| items.values().filter_map(Entity::fields))
    }

    /// 窗口集合里，窗口之外、`keep` 不要整行、且 `changed` 不晚于 `sent`（这个 topic 每个在订订阅都已收到的位置，protocol-v5.md §68.0
    /// 第 5 条）的项换成骨架（只留 `outline` 的键；§67）。不发帧、不改 seq 与 `v`：client 看到的内容不变（快照只带窗口）。换掉的项让
    /// `lossy` 抬到它的 `changed`：更早的 cursor 落后到日志之外时拿快照。没有窗口时什么都不做。
    pub fn thin(
        &mut self,
        col: &str,
        outline: &'static [&'static str],
        sent: u64,
        keep: impl Fn(&Fields) -> bool,
    ) {
        let Some(window) = self.window.as_ref().filter(|window| window.col == col) else {
            return;
        };
        let Some(entities) = self.collections.get_mut(col) else {
            return;
        };
        let order = ordered(entities, &window.field, self.outline);
        let outside: Vec<String> = order
            .iter()
            .take(order.len().saturating_sub(window.limit))
            .map(|(_, id)| (*id).clone())
            .collect();
        self.outline = outline;
        for id in outside {
            if let Some(entity) = entities.get_mut(&id)
                && entity.changed <= sent
                && let Body::Full(fields) = &entity.body
                && !keep(fields)
            {
                self.lossy = self.lossy.max(entity.changed);
                entity.body = Body::Outline(outlined(outline, fields));
            }
        }
    }

    /// 往前补的项挂进集合（protocol-v5.md §73.3）：不发帧、不改 seq。`v` 是当前 seq（命令的 `expect` 拿它比，答案里给的也是它）；`changed` 与
    /// `born` 是 0：续传（§68.3）不会把它们当成「cursor 之后变过的」发给任何订阅，client 只从 `rows.range` 的答案拿到它们。`keep` 为真的留整行，
    /// 其余只留骨架（同 [`Self::thin`] 的键）。已在集合里的项不动。
    pub fn extend(
        &mut self,
        col: &str,
        outline: &'static [&'static str],
        items: Vec<(String, Fields, bool)>,
    ) {
        self.outline = outline;
        let v = self.seq;
        let entities = self.collections.entry(col.to_owned()).or_default();
        for (id, fields, keep) in items {
            let body = if keep {
                Body::Full(fields)
            } else {
                Body::Outline(outlined(outline, &fields))
            };
            entities.entry(id).or_insert(Entity {
                v,
                changed: 0,
                born: 0,
                body,
            });
        }
    }

    /// 一项的版本（`expect` 比对，02 §3）。
    pub fn version(&self, col: &str, id: &str) -> Option<u64> {
        Some(self.collections.get(col)?.get(id)?.v)
    }

    /// 发布一批 `state.set`（按键浅合并）。去掉值没有变的键；全都没变时不发布，返回假。
    pub fn set_state(&mut self, patch: BTreeMap<String, Value>) -> bool {
        let mut batch = Batch::default();
        for (key, value) in patch {
            batch.state(&key, value);
        }
        self.publish(batch)
    }

    /// 发布一批 `entity.put`（整项替换）。集合在第一次 put 时出现（即使这一批是空的），快照里就有这个空集合。
    pub fn put(&mut self, col: &str, items: Vec<(String, Fields)>) -> bool {
        self.collections.entry(col.to_owned()).or_default();
        let mut batch = Batch::default();
        for (id, fields) in items {
            batch.put(col, &id, fields);
        }
        self.publish(batch)
    }

    /// 发布一批：按顺序应用，去掉没有变化的 op；有变化时 seq 加一、记下这一批，返回真。这一批里 put 的项的 `v` 都是这一批
    /// 的 seq（实体版本的规则，protocol-v5.md §14.3）；`entity.append` 不改 `v`（02 §2）。追加到不在的项或不是字符串的字段
    /// 是投影的编程错误：照样发出（client 按 02 §2.2 当 no-op），内容不变。
    pub fn publish(&mut self, batch: Batch) -> bool {
        let seq = self.seq + 1;
        let mut touches = Vec::new();
        for op in batch.ops {
            match op {
                Op::State(key, value) => {
                    if self.state.get(&key) != Some(&value) {
                        self.state.insert(key.clone(), value);
                        self.state_changed.insert(key.clone(), seq);
                        touches.push(Touch::State(key));
                    }
                }
                Op::Put(col, id, fields) => {
                    if let Some(touch) = self.put_one(col, id, fields, seq) {
                        touches.push(touch);
                    }
                }
                Op::Append {
                    col,
                    id,
                    path,
                    chunk,
                } => {
                    if chunk.is_empty() {
                        continue;
                    }
                    // 只剩骨架的项不追加内容（骨架只给不再变的项，追加不会落到它上面，§67.0 第 3 条）
                    if let Some(entity) = self
                        .collections
                        .get_mut(&col)
                        .and_then(|entities| entities.get_mut(&id))
                    {
                        entity.changed = seq;
                        if let Body::Full(fields) = &mut entity.body
                            && let Some(Value::String(text)) = fields.get_mut(&path)
                        {
                            text.push_str(&chunk);
                        }
                    }
                    touches.push(Touch::Append {
                        col,
                        id,
                        path,
                        chunk,
                    });
                }
                Op::Remove(col, id) => {
                    let removed = self
                        .collections
                        .get_mut(&col)
                        .and_then(|entities| entities.remove(&id));
                    if removed.is_some() {
                        if let Some(evicted) = self.tombstones.insert(&col, &id, seq) {
                            self.lossy = self.lossy.max(evicted);
                        }
                        touches.push(Touch::Remove(col, id));
                    }
                }
            }
        }
        if touches.is_empty() {
            return false;
        }
        self.seq = seq;
        self.log.push(seq - 1, seq, touches);
        true
    }

    /// 一项的 put：不在或只剩骨架 → 整行；已是整行 → 按顶层键比，没变不记，变了只记变了的键（线上的 `entity.patch`，01 §19.4）。
    fn put_one(&mut self, col: String, id: String, fields: Fields, seq: u64) -> Option<Touch> {
        let entities = self.collections.entry(col.clone()).or_default();
        self.tombstones.remove(&col, &id);
        let Some(entity) = entities.get_mut(&id) else {
            entities.insert(id.clone(), Entity::full(seq, seq, fields));
            return Some(Touch::Put(col, id));
        };
        let Body::Full(current) = &entity.body else {
            entity.body = Body::Full(fields);
            entity.v = seq;
            entity.changed = seq;
            return Some(Touch::Put(col, id));
        };
        let mut keys: Vec<String> = fields
            .iter()
            .filter(|(key, value)| current.get(*key) != Some(*value))
            .map(|(key, _)| key.clone())
            .collect();
        keys.extend(
            current
                .keys()
                .filter(|key| !fields.contains_key(*key))
                .cloned(),
        );
        if keys.is_empty() {
            return None;
        }
        entity.body = Body::Full(fields);
        entity.v = seq;
        entity.changed = seq;
        Some(Touch::Patch { col, id, keys })
    }

    /// 整份换成 `content`（protocol-v5.md §15.3.6，激活与改写历史）：集合全相同时只发变了的 state 键（一批），返回假；否则
    /// seq 加一、清空续传的账（同一 epoch 的新快照，02 §2：落后的 cursor 只能拿快照），返回真。新内容的项版本都是这个 seq。
    pub fn adopt(&mut self, content: Content) -> bool {
        if !content.outline.is_empty() {
            self.outline = content.outline;
        }
        let empty = BTreeMap::new();
        let outlined_in = |col: &str| content.outlined.get(col).unwrap_or(&empty);
        let same = content.collections.len() == self.collections.len()
            && content
                .outlined
                .keys()
                .all(|col| content.collections.contains_key(col))
            && content.collections.iter().all(|(col, items)| {
                let outlined = outlined_in(col);
                self.collections.get(col).is_some_and(|current| {
                    current.len() == items.len() + outlined.len()
                        && items.iter().all(|(id, fields)| {
                            current
                                .get(id)
                                .is_some_and(|e| e.matches(self.outline, fields))
                        })
                        && outlined.iter().all(|(id, values)| {
                            current
                                .get(id)
                                .is_some_and(|e| e.matches_outline(self.outline, values))
                        })
                })
            });
        if same {
            self.set_state(content.state);
            return false;
        }
        self.seq += 1;
        let seq = self.seq;
        self.rewritten = seq;
        self.lossy = seq;
        self.state_changed = content.state.keys().map(|key| (key.clone(), seq)).collect();
        self.state = content.state;
        let mut outlined = content.outlined;
        self.collections = content
            .collections
            .into_iter()
            .map(|(col, items)| {
                let mut items: BTreeMap<String, Entity> = items
                    .into_iter()
                    .map(|(id, fields)| (id, Entity::full(seq, seq, fields)))
                    .collect();
                for (id, values) in outlined.remove(&col).unwrap_or_default() {
                    let mut entity = Entity::full(seq, seq, Fields::new());
                    entity.body = Body::Outline(values);
                    items.insert(id, entity);
                }
                (col, items)
            })
            .collect();
        self.log.clear();
        self.tombstones.clear();
        true
    }

    /// 快照里的集合：每项是扁平的 `{id, v, ...字段}`（同 TopicStore 的实体）；有窗口的集合只带尾部（行数与字节两个预算，至少一项）。
    fn snapshot_collections(&self) -> BTreeMap<String, Vec<Value>> {
        self.collections
            .iter()
            .map(|(col, entities)| {
                // 窗口里的都是整行：骨架只给窗口外的项，窗口最多退回上一次换骨架时的尾部（protocol-v5.md §67.3）
                let items = match &self.window {
                    Some(window) if window.col == *col => {
                        let ordered = ordered(entities, &window.field, self.outline);
                        let mut tail = Vec::new();
                        let mut bytes = 0;
                        for (_, id) in ordered.iter().rev().take(window.limit) {
                            let Some(entity) = entities.get(*id) else {
                                continue;
                            };
                            let item = flat(id, entity, self.outline);
                            let size = json_bytes(&item);
                            if !tail.is_empty() && bytes + size > window.max_bytes {
                                break;
                            }
                            bytes += size;
                            tail.push(item);
                        }
                        tail.reverse();
                        if let Some(opens) = window.opens {
                            start_at_opening(&mut tail, opens);
                        }
                        tail
                    }
                    _ => entities
                        .iter()
                        .map(|(id, entity)| flat(id, entity, self.outline))
                        .collect(),
                };
                (col.clone(), items)
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "publisher_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "publisher_thin_tests.rs"]
mod thin_tests;

#[cfg(test)]
#[path = "publisher_resume_tests.rs"]
mod resume_tests;
