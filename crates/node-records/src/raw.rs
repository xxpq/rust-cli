//! 读出来还没解码的消息与 part 行（`Rows::messages_raw`，docs/specs/rust-cli/node-store.md §5.4）：消息行已解码（条数少、每条小），part 行只留
//! data 原文。冷读取先在骨架上选分支，再逐条消息整份解码，峰值里不再有整个会话的 part 的 JSON 树。

use std::fmt;

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::codecs::decode_part;
use crate::row::WithParts;

/// part 的 `type` 是 compaction 时才整份解码进骨架（分支选择要它的边界与保留段，branch.rs）。
const COMPACTION: &str = "compaction";
const TYPE_KEY: &str = "type";

/// 一条 part 行：id 与 data 列的原文。
#[derive(Clone, Debug, PartialEq)]
pub struct RawPart {
    pub id: String,
    pub data: String,
}

/// 一条消息行（已解码，同 [`WithParts::info`]）与它的 part 行（原文，按 Node 的行序）。
#[derive(Clone, Debug, PartialEq)]
pub struct RawRecord {
    pub info: Value,
    /// part 的 `sessionID`：读的就是这个会话的行。
    pub session: String,
    pub parts: Vec<RawPart>,
}

impl RawRecord {
    pub fn id(&self) -> &str {
        self.info["id"].as_str().unwrap_or("")
    }

    /// 整份解码（Node `decodePartRow`），同以前的 `Rows::messages`。
    pub fn decode(&self) -> serde_json::Result<WithParts> {
        let parts = self
            .parts
            .iter()
            .map(|part| decode_part(&part.data, &part.id, &self.session, self.id()))
            .collect::<serde_json::Result<_>>()?;
        Ok(WithParts {
            info: self.info.clone(),
            parts,
        })
    }

    /// 骨架：消息照旧，part 只留 compaction 的（整份解码），别的 part 只读出 `type` 就丢下，不建 JSON 树。分支选择（活动历史、earlier、
    /// 保留段）只看消息与 compaction part，在骨架上的结果与在整份记录上的相同。data 不是合法 JSON 时同整份解码一样报错。
    pub fn skeleton(&self) -> serde_json::Result<WithParts> {
        let mut parts = Vec::new();
        for part in &self.parts {
            let Kind(compaction) = serde_json::from_str(&part.data)?;
            if compaction {
                parts.push(decode_part(&part.data, &part.id, &self.session, self.id())?);
            }
        }
        Ok(WithParts {
            info: self.info.clone(),
            parts,
        })
    }
}

/// 一个 part 的 `type` 是不是 compaction，读法同 `Value`（`part["type"] == "compaction"`）：不是对象时没有 `type`；同一个键出现两次取后一个。
/// 其余成员跳过（`IgnoredAny`）。
struct Kind(bool);

impl<'de> Deserialize<'de> for Kind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(KindVisitor)
    }
}

struct KindVisitor;

impl<'de> Visitor<'de> for KindVisitor {
    type Value = Kind;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Kind, A::Error> {
        let mut compaction = false;
        while let Some(IsType(is_type)) = map.next_key()? {
            if is_type {
                compaction = map.next_value::<Value>()? == COMPACTION;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Kind(compaction))
    }

    // 不是对象的 data：解码时是空对象（codecs 的 `object`），没有 type
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Kind, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Kind(false))
    }

    fn visit_bool<E>(self, _: bool) -> Result<Kind, E> {
        Ok(Kind(false))
    }

    fn visit_i64<E>(self, _: i64) -> Result<Kind, E> {
        Ok(Kind(false))
    }

    fn visit_u64<E>(self, _: u64) -> Result<Kind, E> {
        Ok(Kind(false))
    }

    fn visit_f64<E>(self, _: f64) -> Result<Kind, E> {
        Ok(Kind(false))
    }

    fn visit_str<E>(self, _: &str) -> Result<Kind, E> {
        Ok(Kind(false))
    }

    fn visit_unit<E>(self) -> Result<Kind, E> {
        Ok(Kind(false))
    }
}

/// 一条消息行的头（`Rows::message_heads`，node-store.md §5.6）：建历史索引只要 id、角色与 `summary` 是不是真，不建整份 info。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageHead {
    pub id: String,
    /// data 里的 `role`（不是字符串时为 None：分支与片段只拿它与 user / assistant 比）。
    pub role: Option<String>,
    /// data 里的 `summary` 按 JavaScript 的真值（压缩摘要的标记，[`crate::branch::truthy`]）。
    pub summary: bool,
}

/// `role` 与 `summary` 这两个键。
const ROLE_KEY: &str = "role";
const SUMMARY_KEY: &str = "summary";

impl MessageHead {
    /// 从 data 列的原文读：同 `decode_message` 之后读这两个成员——data 不是对象时都没有；同一个键出现两次取后一个；其余成员跳过（`IgnoredAny`），
    /// 不建 JSON 树；不是合法 JSON 时报错。
    pub fn parse(id: String, data: &str) -> serde_json::Result<Self> {
        let Head { role, summary } = serde_json::from_str(data)?;
        Ok(Self { id, role, summary })
    }
}

/// [`MessageHead`] 里从 data 读出的两项。
struct Head {
    role: Option<String>,
    summary: bool,
}

impl<'de> Deserialize<'de> for Head {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(HeadVisitor)
    }
}

struct HeadVisitor;

impl HeadVisitor {
    fn none() -> Head {
        Head {
            role: None,
            summary: false,
        }
    }
}

impl<'de> Visitor<'de> for HeadVisitor {
    type Value = Head;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Head, A::Error> {
        let mut head = Self::none();
        while let Some(HeadKey(key)) = map.next_key()? {
            match key {
                Some(ROLE_KEY) => {
                    head.role = map.next_value::<Value>()?.as_str().map(str::to_owned);
                }
                Some(SUMMARY_KEY) => {
                    head.summary = crate::branch::truthy(Some(&map.next_value::<Value>()?));
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(head)
    }

    // 不是对象的 data：解码时是空对象（codecs 的 `object`），两项都没有
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Head, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Self::none())
    }

    fn visit_bool<E>(self, _: bool) -> Result<Head, E> {
        Ok(Self::none())
    }

    fn visit_i64<E>(self, _: i64) -> Result<Head, E> {
        Ok(Self::none())
    }

    fn visit_u64<E>(self, _: u64) -> Result<Head, E> {
        Ok(Self::none())
    }

    fn visit_f64<E>(self, _: f64) -> Result<Head, E> {
        Ok(Self::none())
    }

    fn visit_str<E>(self, _: &str) -> Result<Head, E> {
        Ok(Self::none())
    }

    fn visit_unit<E>(self) -> Result<Head, E> {
        Ok(Self::none())
    }
}

/// 一个键是 `role`、`summary` 还是别的（None）：只比较，不另存键。
struct HeadKey(Option<&'static str>);

impl<'de> Deserialize<'de> for HeadKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(HeadKeyVisitor)
    }
}

struct HeadKeyVisitor;

impl Visitor<'_> for HeadKeyVisitor {
    type Value = HeadKey;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an object key")
    }

    fn visit_str<E>(self, key: &str) -> Result<HeadKey, E> {
        Ok(HeadKey(
            [ROLE_KEY, SUMMARY_KEY]
                .into_iter()
                .find(|known| *known == key),
        ))
    }
}

/// 一个键是不是 `type`：只比较，不另存键（借用或转义后的临时串都行）。
struct IsType(bool);

impl<'de> Deserialize<'de> for IsType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(IsTypeVisitor)
    }
}

struct IsTypeVisitor;

impl Visitor<'_> for IsTypeVisitor {
    type Value = IsType;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an object key")
    }

    fn visit_str<E>(self, key: &str) -> Result<IsType, E> {
        Ok(IsType(key == TYPE_KEY))
    }
}

#[cfg(test)]
#[path = "raw_tests.rs"]
mod tests;
