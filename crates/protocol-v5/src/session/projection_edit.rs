//! 投影的一次改动（protocol-v5.md §15.2）：一批 op，加上同一批里还没发布的项。从 `projection.rs` 拆出（它到了 800 行上限），
//! 行为不变；投影与它的子模块经 `super::Edit` 用它。

use std::collections::BTreeMap;

use serde_json::Value;

use super::{INPUTS, QUEUE, WORKS};
use crate::publisher::{Batch, Content, Fields, Publisher};
use crate::session::interactions::INTERACTIONS;
use crate::session::rows::ROWS;

/// 一次事件（或一次建投影）的改动：一批 op，加上同一批里还没发布的项（同一事件里先建后改的行读得到它）。
pub(super) struct Edit<'a> {
    base: Option<&'a Publisher>,
    /// 要发布的 op；建投影的改动（[`Self::building`]）没有：内容整份由 [`Self::into_content`] 交出，op 用不上。
    ///
    /// 缺陷修复（MEM-V5，protocol-v5.md §66.5）：以前建投影也记 op，每次 put 都把整行再复制一份进 op；一个工具行 put 三次（调用、
    /// 结果、起止），op 里留着两份带全文的副本，建完才随 op 丢掉，打开长会话时峰值是常驻的三四倍。
    batch: Option<Batch>,
    items: BTreeMap<(&'static str, String), Option<Fields>>,
    /// 建的时候就换成骨架的项（冷读只建窗口，protocol-v5.md §71.3）：`outline` 那几个键的值。只有建投影的改动有。
    outlined: BTreeMap<(&'static str, String), Box<[Value]>>,
    outline: &'static [&'static str],
    state: BTreeMap<&'static str, Value>,
}

impl<'a> Edit<'a> {
    /// 发布用的改动：记 op（增量要带整项；这一步发布之后 op 与 `items` 一起丢）。
    pub(super) fn new(base: Option<&'a Publisher>) -> Self {
        Self {
            base,
            batch: Some(Batch::default()),
            items: BTreeMap::new(),
            outlined: BTreeMap::new(),
            outline: &[],
            state: BTreeMap::new(),
        }
    }

    /// 建投影用的改动（`Projection::build`）：只收内容、不记 op，每一项只存一份。
    pub(super) fn building() -> Self {
        Self {
            base: None,
            batch: None,
            items: BTreeMap::new(),
            outlined: BTreeMap::new(),
            outline: &[],
            state: BTreeMap::new(),
        }
    }

    /// 读一项，不复制（要改的地方自己 `.cloned()` 再 [`Self::put`]）。
    ///
    /// 缺陷修复（MEM-V5，§66.5）：以前每次读都把整行（带工具的全文）复制出来，只看一个字段的地方（行的序号、在不在）也复制。
    pub(super) fn get(&self, col: &'static str, id: &str) -> Option<&Fields> {
        match self.items.get(&(col, id.to_owned())) {
            Some(item) => item.as_ref(),
            None => self.base.and_then(|base| base.entity(col, id)),
        }
    }

    /// 一项的一个字段：窗口外只剩骨架的项也有（行号在骨架里，§67.0 第 1 条），所以只读行号的地方用它。
    pub(super) fn field(&self, col: &'static str, id: &str, key: &str) -> Option<&Value> {
        let item = (col, id.to_owned());
        if let Some(values) = self.outlined.get(&item) {
            return values.get(self.outline.iter().position(|k| *k == key)?);
        }
        match self.items.get(&item) {
            Some(item) => item.as_ref()?.get(key),
            None => self.base?.field(col, id, key),
        }
    }

    pub(super) fn put(&mut self, col: &'static str, id: &str, fields: Fields) {
        if let Some(batch) = &mut self.batch {
            batch.put(col, id, fields.clone());
        }
        let item = (col, id.to_owned());
        // 建的时候已换成骨架的项再 put：仍是骨架（骨架只给不再变的项，种类、回合与行号不会变）
        if let Some(values) = self.outlined.get_mut(&item) {
            *values = outline_of(self.outline, &fields);
            return;
        }
        self.items.insert(item, Some(fields));
    }

    /// 建投影时把一项换成骨架（冷读只建窗口，protocol-v5.md §71.3）：只留 `keys` 的值，之后读整行的地方读不到它（同 Publisher 的骨架）。
    pub(super) fn outline(&mut self, col: &'static str, id: &str, keys: &'static [&'static str]) {
        let item = (col, id.to_owned());
        if let Some(Some(fields)) = self.items.remove(&item) {
            self.outline = keys;
            self.outlined.insert(item, outline_of(keys, &fields));
        }
    }

    /// 改一项（不在时不动）：建投影时就地改，不复制；发布用的改动在这一步的那一份上改（还没有就从底子复制一份），改完的整项进 op。
    ///
    /// 缺陷修复（MEM-V5，§66.5）：以前「读一份、改、再写」在建投影时把每个工具行整行（带全文）复制两次（结果一次、起止一次）。
    pub(super) fn update(&mut self, col: &'static str, id: &str, change: impl FnOnce(&mut Fields)) {
        let key = (col, id.to_owned());
        if !self.items.contains_key(&key) {
            let Some(base) = self.base.and_then(|base| base.entity(col, id)) else {
                return;
            };
            self.items.insert(key.clone(), Some(base.clone()));
        }
        let Some(Some(fields)) = self.items.get_mut(&key) else {
            return;
        };
        change(fields);
        if let Some(batch) = &mut self.batch {
            batch.put(col, id, fields.clone());
        }
    }

    pub(super) fn append(&mut self, col: &'static str, id: &str, path: &str, chunk: &str) {
        if let Some(batch) = &mut self.batch {
            batch.append(col, id, path, chunk);
        }
        if let Some(Some(fields)) = self.items.get_mut(&(col, id.to_owned()))
            && let Some(Value::String(text)) = fields.get_mut(path)
        {
            text.push_str(chunk);
        }
    }

    pub(super) fn remove(&mut self, col: &'static str, id: &str) {
        if let Some(batch) = &mut self.batch {
            batch.remove(col, id);
        }
        self.outlined.remove(&(col, id.to_owned()));
        self.items.insert((col, id.to_owned()), None);
    }

    pub(super) fn state(&mut self, key: &'static str, value: Value) {
        if let Some(batch) = &mut self.batch {
            batch.state(key, value.clone());
        }
        self.state.insert(key, value);
    }

    /// 这一次事件的 op（交给出站队列发布）。建投影的改动没有 op，是空的一批。
    pub(super) fn into_batch(self) -> Batch {
        self.batch.unwrap_or_default()
    }

    /// 建投影时的内容（没有底子：全部项都在 `items` 里）。
    pub(super) fn into_content(self) -> Content {
        let mut content = Content::default();
        for col in [ROWS, QUEUE, INTERACTIONS, WORKS, INPUTS] {
            content.collections.entry(col.to_owned()).or_default();
        }
        for ((col, id), fields) in self.items {
            if let Some(fields) = fields {
                content
                    .collections
                    .entry(col.to_owned())
                    .or_default()
                    .insert(id, fields);
            }
        }
        for ((col, id), values) in self.outlined {
            content
                .outlined
                .entry(col.to_owned())
                .or_default()
                .insert(id, values);
        }
        content.outline = self.outline;
        content.state = self
            .state
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        content
    }
}

/// 一项的骨架：`keys` 里每个键的值（没有的键是 null，同 Publisher 的骨架）。
fn outline_of(keys: &[&str], fields: &Fields) -> Box<[Value]> {
    keys.iter()
        .map(|key| fields.get(*key).cloned().unwrap_or(Value::Null))
        .collect()
}

#[cfg(test)]
#[path = "projection_edit_tests.rs"]
mod tests;
