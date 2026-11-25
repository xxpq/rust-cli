//! 续传帧怎么拼（01 §19.2，protocol-v5.md §68.3）：保留日志只记每批谁变了、追加的块；帧从当前内容拼——同一实体至多一条 put / remove，
//! 或一条 patch 加几条不同路径的 append；落后到日志之外时按实体的变更序号（`changed > cursor`）拼。拼不出时给快照。
//! 与 protocol-runtime `store/publisher.ts` 同一规则（那边的副本另记 patch 的值，rust 没有副本，不用）。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::Value;

use super::{Body, Entity, Publisher};
use crate::generated::core_v1::{
    Cursor, DeltaOp, DeltaOpEntityAppend, DeltaOpEntityPatch, DeltaOpEntityPut,
    DeltaOpEntityRemove, DeltaOpStateSet, Frame, FrameBody, FrameBodyDeltas, FrameBodySnapshot,
    Snapshot,
};

/// 一条日志的固定开销（不碰任何实体的条目也占预算）。
const ENTRY_OVERHEAD_BYTES: usize = 16;

/// 一批碰到的东西：put / remove 只记键；patch 记变了的键（值从当前内容取）；append 记块。
#[derive(Debug)]
pub(super) enum Touch {
    Put(String, String),
    Patch {
        col: String,
        id: String,
        keys: Vec<String>,
    },
    Append {
        col: String,
        id: String,
        path: String,
        chunk: String,
    },
    Remove(String, String),
    State(String),
}

impl Touch {
    fn bytes(&self) -> usize {
        match self {
            Touch::Put(col, id) | Touch::Remove(col, id) => col.len() + id.len(),
            Touch::Patch { col, id, keys } => {
                col.len() + id.len() + keys.iter().map(String::len).sum::<usize>()
            }
            Touch::Append {
                col,
                id,
                path,
                chunk,
            } => col.len() + id.len() + path.len() + chunk.len(),
            Touch::State(key) => key.len(),
        }
    }
}

/// 一条保留日志：覆盖 `(from, to]`（本进程发布的批 `to = from + 1`）。
#[derive(Debug)]
struct Entry {
    from: u64,
    to: u64,
    touches: Vec<Touch>,
    bytes: usize,
}

/// 保留日志：按字节预算丢最早的，至少留最新一条。
#[derive(Debug)]
pub(super) struct Log {
    entries: VecDeque<Entry>,
    bytes: usize,
    budget: usize,
}

impl Log {
    pub(super) fn new(budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    pub(super) fn push(&mut self, from: u64, to: u64, touches: Vec<Touch>) {
        let bytes = touches
            .iter()
            .map(Touch::bytes)
            .sum::<usize>()
            .saturating_add(ENTRY_OVERHEAD_BYTES);
        self.entries.push_back(Entry {
            from,
            to,
            touches,
            bytes,
        });
        self.bytes += bytes;
        while self.entries.len() > 1 && self.bytes > self.budget {
            if let Some(dropped) = self.entries.pop_front() {
                self.bytes -= dropped.bytes;
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// 日志的下界：最早一条的起点；空时为 None（= 当前 seq）。
    pub(super) fn floor(&self) -> Option<u64> {
        self.entries.front().map(|entry| entry.from)
    }
}

/// 墓碑：删掉的项 → 删它的 seq；按条数限，丢掉最早的。
#[derive(Debug)]
pub(super) struct Tombstones {
    seqs: BTreeMap<(String, String), u64>,
    /// 插入顺序（同一项再删时旧的那条留着，弹出时按 seq 认出已经过时）。
    order: VecDeque<((String, String), u64)>,
    budget: usize,
}

impl Tombstones {
    pub(super) fn new(budget: usize) -> Self {
        Self {
            seqs: BTreeMap::new(),
            order: VecDeque::new(),
            budget,
        }
    }

    /// 记一条；超了预算时丢最早的，返回丢掉的那条的 seq（调用方据此抬 `lossy`）。
    pub(super) fn insert(&mut self, col: &str, id: &str, seq: u64) -> Option<u64> {
        let key = (col.to_owned(), id.to_owned());
        self.seqs.insert(key.clone(), seq);
        self.order.push_back((key, seq));
        let mut evicted = None;
        while self.seqs.len() > self.budget {
            let Some((key, seq)) = self.order.pop_front() else {
                break;
            };
            if self.seqs.get(&key) == Some(&seq) {
                self.seqs.remove(&key);
                evicted = Some(seq);
            }
        }
        evicted
    }

    /// 同一项又建了：它不再是删掉的。
    pub(super) fn remove(&mut self, col: &str, id: &str) {
        self.seqs.remove(&(col.to_owned(), id.to_owned()));
    }

    pub(super) fn clear(&mut self) {
        self.seqs.clear();
        self.order.clear();
    }

    fn contains(&self, col: &str, id: &str) -> bool {
        self.seqs.contains_key(&(col.to_owned(), id.to_owned()))
    }
}

/// 日志路里一个实体的累积（按第一次碰到的顺序）。
struct Acc {
    col: String,
    id: String,
    /// 有 put：要当前整行。
    whole: bool,
    removed: bool,
    /// 被 patch 过的键（第一次出现的顺序）。
    patched: Vec<String>,
    /// 路径 → 依次追加的块拼起来（第一次出现的顺序）。
    appends: Vec<(String, String)>,
}

fn op(delta: DeltaOp) -> Value {
    serde_json::to_value(delta).unwrap_or(Value::Null)
}

impl Publisher {
    /// 给 cursor（订阅已经有的位置）的帧：同一 epoch 里拼得出续传帧就给增量（01 §19.2），否则快照。`patch`：这条连接协商出
    /// `core.patch`（01 §19.4），否则同一项发当前整行的 put。
    pub fn frame_for(&self, cursor: Option<&Cursor>, sub: &str, patch: bool) -> Frame {
        let resumed = cursor
            .filter(|at| at.epoch == self.epoch)
            .and_then(|at| Some((at.seq, self.resume(at.seq, patch)?)));
        let (from, body) = match resumed {
            Some((from, deltas)) => (from, FrameBody::Deltas(FrameBodyDeltas { deltas })),
            None => (
                0,
                FrameBody::Snapshot(FrameBodySnapshot {
                    snapshot: Snapshot {
                        state: self.state.clone(),
                        collections: self.snapshot_collections(),
                    },
                }),
            ),
        };
        Frame {
            topic: self.topic.clone(),
            sub: sub.to_owned(),
            epoch: self.epoch.clone(),
            from,
            to: self.seq,
            body,
        }
    }

    /// 续传的 op；拼不出（要快照）时为 None。
    fn resume(&self, c: u64, patch: bool) -> Option<Vec<Value>> {
        if c < self.rewritten || c > self.seq {
            return None;
        }
        if c == self.seq {
            return Some(Vec::new());
        }
        if c >= self.log.floor().unwrap_or(self.seq) {
            return self.by_log(c, patch);
        }
        if c < self.lossy {
            return None;
        }
        self.by_changes(c)
    }

    /// 日志路：按日志里碰到的实体，从当前内容拼。
    fn by_log(&self, c: u64, patch: bool) -> Option<Vec<Value>> {
        let mut accs: Vec<Acc> = Vec::new();
        let mut index: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        let mut state = BTreeSet::new();
        for entry in self.log.entries.iter().filter(|entry| entry.to > c) {
            // cursor 落在这一条中间：拆不开，碰到的实体都从当前内容整项给（本进程的条目各一个 seq，不会出现；规则与副本相同）
            let straddle = entry.from < c;
            for touch in &entry.touches {
                let (col, id) = match touch {
                    Touch::State(key) => {
                        state.insert(key.as_str());
                        continue;
                    }
                    Touch::Put(col, id) | Touch::Remove(col, id) => (col, id),
                    Touch::Patch { col, id, .. } | Touch::Append { col, id, .. } => (col, id),
                };
                let at = *index.entry((col, id)).or_insert_with(|| {
                    accs.push(Acc {
                        col: col.clone(),
                        id: id.clone(),
                        whole: false,
                        removed: false,
                        patched: Vec::new(),
                        appends: Vec::new(),
                    });
                    accs.len() - 1
                });
                let acc = &mut accs[at];
                acc.whole |= straddle;
                match touch {
                    Touch::Put(..) => acc.whole = true,
                    Touch::Remove(..) => acc.removed = true,
                    Touch::Patch { keys, .. } => {
                        for key in keys {
                            if !acc.patched.contains(key) {
                                acc.patched.push(key.clone());
                            }
                        }
                    }
                    Touch::Append { path, chunk, .. } => {
                        match acc.appends.iter_mut().find(|(p, _)| p == path) {
                            Some((_, text)) => text.push_str(chunk),
                            None => acc.appends.push((path.clone(), chunk.clone())),
                        }
                    }
                    Touch::State(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        for acc in &accs {
            out.extend(self.entity_ops(acc, patch)?);
        }
        if !state.is_empty() {
            out.push(self.state_set(state));
        }
        Some(out)
    }

    fn entity_ops(&self, acc: &Acc, patch: bool) -> Option<Vec<Value>> {
        let (col, id) = (acc.col.clone(), acc.id.clone());
        let Some(entity) = self.collections.get(&acc.col).and_then(|e| e.get(&acc.id)) else {
            if acc.removed || self.tombstones.contains(&acc.col, &acc.id) {
                return Some(vec![op(DeltaOp::EntityRemove(DeltaOpEntityRemove {
                    col,
                    id,
                }))]);
            }
            // 追加到不在的项（投影的编程错误，§68.3）：照样转发，client 当 no-op；要整行就拼不出
            if acc.whole || !acc.patched.is_empty() {
                return None;
            }
            return Some(self.appends(acc, &[]));
        };
        let Body::Full(fields) = &entity.body else {
            // 骨架只给每个在订订阅都已收到的行（§68.0 第 5 条）：要到它的内容说明订阅者落后了，给快照
            return None;
        };
        if acc.whole || (!acc.patched.is_empty() && !patch) {
            return Some(vec![put(&acc.col, &acc.id, entity, fields)]);
        }
        let mut out = Vec::new();
        if !acc.patched.is_empty() {
            let mut set = BTreeMap::new();
            let mut unset = Vec::new();
            for key in &acc.patched {
                match fields.get(key) {
                    Some(value) => {
                        set.insert(key.clone(), value.clone());
                    }
                    None => unset.push(key.clone()),
                }
            }
            out.push(op(DeltaOp::EntityPatch(DeltaOpEntityPatch {
                col,
                id,
                v: entity.v,
                fields: set,
                unset: (!unset.is_empty()).then_some(unset),
            })));
        }
        out.extend(self.appends(acc, &acc.patched));
        Some(out)
    }

    /// 没被 patch 过的路径各一条 append（被 patch 过的取的是当前值，已含之后的追加）。
    fn appends(&self, acc: &Acc, patched: &[String]) -> Vec<Value> {
        acc.appends
            .iter()
            .filter(|(path, _)| !patched.contains(path))
            .map(|(path, chunk)| {
                op(DeltaOp::EntityAppend(DeltaOpEntityAppend {
                    col: acc.col.clone(),
                    id: acc.id.clone(),
                    path: path.clone(),
                    chunk: chunk.clone(),
                }))
            })
            .collect()
    }

    /// 变更序号路：`changed > c` 的项整行 put、墓碑 remove、state 键 state.set；窗口集合里落后期间新出现的项多于一个窗口时给快照。
    fn by_changes(&self, c: u64) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        for (col, entities) in &self.collections {
            let window = self.window.as_ref().filter(|window| window.col == *col);
            let mut born = 0;
            for (id, entity) in entities.iter().filter(|(_, e)| e.changed > c) {
                if let Some(window) = window
                    && entity.born > c
                {
                    born += 1;
                    if born > window.limit {
                        return None;
                    }
                }
                let Body::Full(fields) = &entity.body else {
                    return None;
                };
                out.push(put(col, id, entity, fields));
            }
        }
        for ((col, id), seq) in &self.tombstones.seqs {
            let present = self
                .collections
                .get(col)
                .is_some_and(|e| e.contains_key(id));
            if *seq > c && !present {
                out.push(op(DeltaOp::EntityRemove(DeltaOpEntityRemove {
                    col: col.clone(),
                    id: id.clone(),
                })));
            }
        }
        let state: BTreeSet<&str> = self
            .state_changed
            .iter()
            .filter(|(_, seq)| **seq > c)
            .map(|(key, _)| key.as_str())
            .collect();
        if !state.is_empty() {
            out.push(self.state_set(state));
        }
        Some(out)
    }

    fn state_set(&self, keys: BTreeSet<&str>) -> Value {
        let patch = keys
            .into_iter()
            .filter_map(|key| Some((key.to_owned(), self.state.get(key)?.clone())))
            .collect();
        op(DeltaOp::StateSet(DeltaOpStateSet { patch }))
    }
}

fn put(col: &str, id: &str, entity: &Entity, fields: &super::Fields) -> Value {
    op(DeltaOp::EntityPut(DeltaOpEntityPut {
        col: col.to_owned(),
        id: id.to_owned(),
        v: entity.v,
        data: fields.clone(),
    }))
}
