//! 续传帧的等价性（01 §19.2，protocol-v5.md §68.4）：对任意 cursor，把 `frame_for(cursor)` 应用到「那个 cursor 时的内容」等于当前内容。
//! 随机的批（建、整行改、追加、删、state.set），保留预算很小，日志路、变更序号路、快照三条都走到；连接认得 / 不认得 patch 各一遍。
//! 与 protocol-runtime `test/publisherResume.test.ts` 同一组性质。

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::*;
use crate::generated::core_v1::{Frame, FrameBody};

/// 带种子的伪随机（xorshift64*）：失败可复现。
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n
    }
}

const IDS: [&str; 5] = ["a", "b", "c", "d", "e"];
const KEYS: [&str; 3] = ["status", "preview", "note"];

/// client 一侧的内容：state 与集合（集合名 → id → 扁平的 `{id, v, ...}`），照 02 §2.2 应用帧。
#[derive(Debug, Clone, Default, PartialEq)]
struct Client {
    state: BTreeMap<String, Value>,
    collections: BTreeMap<String, BTreeMap<String, Map<String, Value>>>,
}

impl Client {
    fn apply(&mut self, frame: &Frame) {
        match &frame.body {
            FrameBody::Snapshot(snapshot) => {
                self.state = snapshot.snapshot.state.clone();
                self.collections = snapshot
                    .snapshot
                    .collections
                    .iter()
                    .map(|(col, items)| {
                        let items = items
                            .iter()
                            .map(|item| {
                                let item = item.as_object().unwrap().clone();
                                (item["id"].as_str().unwrap().to_owned(), item)
                            })
                            .collect();
                        (col.clone(), items)
                    })
                    .collect();
            }
            FrameBody::Deltas(deltas) => {
                for delta in &deltas.deltas {
                    self.delta(delta);
                }
            }
        }
    }

    fn delta(&mut self, delta: &Value) {
        let text = |key: &str| delta[key].as_str().unwrap().to_owned();
        match delta["op"].as_str().unwrap() {
            "state.set" => {
                for (key, value) in delta["patch"].as_object().unwrap() {
                    self.state.insert(key.clone(), value.clone());
                }
            }
            "entity.put" => {
                let mut item = delta["data"].as_object().unwrap().clone();
                item.insert("id".to_owned(), delta["id"].clone());
                item.insert("v".to_owned(), delta["v"].clone());
                self.collections
                    .entry(text("col"))
                    .or_default()
                    .insert(text("id"), item);
            }
            "entity.patch" => {
                if let Some(item) = self
                    .collections
                    .get_mut(&text("col"))
                    .and_then(|items| items.get_mut(&text("id")))
                {
                    for (key, value) in delta["fields"].as_object().unwrap() {
                        item.insert(key.clone(), value.clone());
                    }
                    for key in delta["unset"].as_array().into_iter().flatten() {
                        item.remove(key.as_str().unwrap());
                    }
                    item.insert("v".to_owned(), delta["v"].clone());
                }
            }
            "entity.append" => {
                if let Some(item) = self
                    .collections
                    .get_mut(&text("col"))
                    .and_then(|items| items.get_mut(&text("id")))
                    && let Some(Value::String(current)) = item.get_mut(&text("path"))
                {
                    current.push_str(delta["chunk"].as_str().unwrap());
                }
            }
            "entity.remove" => {
                if let Some(items) = self.collections.get_mut(&text("col")) {
                    items.remove(&text("id"));
                }
            }
            other => panic!("unknown op {other}"),
        }
    }
}

fn snapshot_of(publisher: &Publisher) -> Client {
    let mut client = Client::default();
    client.apply(&publisher.frame_for(None, "s", false));
    client
}

fn random_batch(rng: &mut Rng, publisher: &Publisher, n: u64) -> Batch {
    let mut batch = Batch::default();
    for _ in 0..=rng.below(3) {
        let id = IDS[rng.below(IDS.len() as u64) as usize];
        let current = publisher.entity("rows", id).cloned();
        match (current, rng.below(10)) {
            (None, _) | (Some(_), 0) => batch.put(
                "rows",
                id,
                BTreeMap::from([
                    ("text".to_owned(), json!(format!("t{n}"))),
                    ("status".to_owned(), json!("s")),
                ]),
            ),
            // 整行 put 只改一个键或删一个键：Publisher 记成 patch
            (Some(mut fields), 1..=3) => {
                let key = KEYS[rng.below(KEYS.len() as u64) as usize];
                if rng.below(3) == 0 {
                    fields.remove(key);
                } else {
                    fields.insert(key.to_owned(), json!(format!("p{n}")));
                }
                batch.put("rows", id, fields);
            }
            (Some(_), 4..=7) => batch.append("rows", id, "text", &format!("{}", n % 10)),
            (Some(_), 8) => batch.remove("rows", id),
            (Some(_), _) => batch.state(&format!("k{}", rng.below(3)), json!(n)),
        }
    }
    batch
}

#[test]
fn every_cursor_resumes_to_the_current_content() {
    let (mut snapshots, mut from_log, mut from_changes) = (0, 0, 0);
    for seed in 1..=200_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut p = Publisher::new("t", "e").with_retention(120, 3);
        p.put("rows", Vec::new());
        let mut history = vec![snapshot_of(&p)];
        for n in 1..=40 {
            let batch = random_batch(&mut rng, &p, n);
            p.publish(batch);
            // 没变化的批不占 seq：同一个 seq 只记一次
            if history.len() as u64 <= p.cursor().seq {
                history.push(snapshot_of(&p));
            }
        }
        let current = snapshot_of(&p);
        for (c, at) in history.iter().enumerate() {
            for patch in [true, false] {
                let frame = p.frame_for(
                    Some(&Cursor {
                        epoch: "e".to_owned(),
                        seq: c as u64,
                    }),
                    "s",
                    patch,
                );
                match &frame.body {
                    FrameBody::Snapshot(_) => snapshots += 1,
                    FrameBody::Deltas(deltas) => {
                        if (c as u64) >= p.log.floor().unwrap_or(p.seq) {
                            from_log += 1;
                        } else {
                            from_changes += 1;
                        }
                        if !patch {
                            assert!(deltas.deltas.iter().all(|d| d["op"] != "entity.patch"));
                        }
                    }
                }
                let mut got = at.clone();
                got.apply(&frame);
                assert_eq!(got, current, "seed {seed} cursor {c} patch {patch}");
            }
        }
    }
    assert!(snapshots > 0 && from_log > 0 && from_changes > 0);
}

/// 已在内存的整行收到只改一个键的 put：认得 patch 的连接收到 patch（变了的键、删掉的键），别的连接收到整行 put；没变的 put 不发。
#[test]
fn a_put_to_a_resident_row_is_sent_as_a_patch() {
    let mut p = Publisher::new("t", "e");
    let row = |status: &str, preview: Option<&str>| {
        let mut fields = BTreeMap::from([
            ("text".to_owned(), json!("hi")),
            ("status".to_owned(), json!(status)),
        ]);
        if let Some(preview) = preview {
            fields.insert("preview".to_owned(), json!(preview));
        }
        fields
    };
    p.put("rows", vec![("r1".to_owned(), row("running", Some("p")))]);
    assert!(!p.put("rows", vec![("r1".to_owned(), row("running", Some("p")))]));
    p.put("rows", vec![("r1".to_owned(), row("done", None))]);
    let at = Cursor {
        epoch: "e".to_owned(),
        seq: 1,
    };
    let FrameBody::Deltas(patched) = p.frame_for(Some(&at), "s", true).body else {
        panic!("not deltas");
    };
    assert_eq!(
        patched.deltas,
        vec![
            json!({"op": "entity.patch", "col": "rows", "id": "r1", "v": 2,
                    "fields": {"status": "done"}, "unset": ["preview"]})
        ]
    );
    let FrameBody::Deltas(whole) = p.frame_for(Some(&at), "s", false).body else {
        panic!("not deltas");
    };
    assert_eq!(
        whole.deltas,
        vec![
            json!({"op": "entity.put", "col": "rows", "id": "r1", "v": 2,
                    "data": {"status": "done", "text": "hi"}})
        ]
    );
}

/// 一次突发的几百个追加落后之后恢复：一条拼起来的 append，不是快照（以前落后 256 批就整份快照，评审 R1）。
#[test]
fn a_burst_resumes_as_one_append() {
    let mut p = Publisher::new("t", "e");
    p.put(
        "rows",
        vec![(
            "r1".to_owned(),
            BTreeMap::from([("text".to_owned(), json!(""))]),
        )],
    );
    for _ in 0..6000 {
        let mut batch = Batch::default();
        batch.append("rows", "r1", "text", "abcd");
        p.publish(batch);
    }
    let at = Cursor {
        epoch: "e".to_owned(),
        seq: 1,
    };
    let FrameBody::Deltas(deltas) = p.frame_for(Some(&at), "s", true).body else {
        panic!("not deltas");
    };
    assert_eq!(
        deltas.deltas,
        vec![
            json!({"op": "entity.append", "col": "rows", "id": "r1", "path": "text",
                    "chunk": "abcd".repeat(6000)})
        ]
    );
}
