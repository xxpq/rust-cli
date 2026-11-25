//! 读端宽松（docs/zcode-protocol-v5/11-contract-codegen.md §5；与 `packages/protocol/src/codec/tolerant.ts` 同一语义）：生成的
//! 类型调用这里。外壳的必填字段严格（serde 照常失败）；带降级值的字段读不出时取降级值；集合里单个实体失败换成占位。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// 不透明 JSON：契约里没有类型的字段（`z.unknown()`、transform 的输入；11 §9）。生成的类型里显式用这个名字，不静默生成成
/// 任意类型；位置列在 `schema/<cap>@<rev>.json` 的 `opaque`。
pub type Opaque = Value;

/// 带降级值的枚举的线上文字：字符串原样；别的值（数字、对象……）取它的 JSON 文字，之后按“不认识的值”降级。
pub fn enum_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => text,
        other => other.to_string(),
    })
}

/// 带降级值的字段（zod 的 `.catch`）：读成 `T` 失败时取 `fallback()`，不让整条消息失败。
pub fn lenient<'de, D, T, F>(deserializer: D, fallback: F) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
    F: FnOnce() -> T,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|_| fallback()))
}

/// 集合里的一项：读出来的实体，或者读不出时的占位（保留能读到的 id 与 v，读不到 id 时用位置编号）。
#[derive(Debug, Clone, PartialEq)]
pub enum Entity<T> {
    Valid(T),
    Invalid { id: String, v: u64, reason: String },
}

/// 读不到 id 时占位的前缀（`invalid-<位置>`，同 tolerant.ts）。
const INVALID_ID_PREFIX: &str = "invalid-";

/// 集合解码（tolerant.ts 的 `decodeCollection`）：逐项读，失败项换成占位，其余照常；不是数组时按空集合。
pub fn decode_collection<T: DeserializeOwned>(raw: &Value) -> Vec<Entity<T>> {
    let Some(items) = raw.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .map(
            |(index, item)| match serde_json::from_value::<T>(item.clone()) {
                Ok(entity) => Entity::Valid(entity),
                Err(error) => Entity::Invalid {
                    id: item
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map_or_else(|| format!("{INVALID_ID_PREFIX}{index}"), str::to_owned),
                    v: item.get("v").and_then(Value::as_u64).unwrap_or_default(),
                    reason: error.to_string(),
                },
            },
        )
        .collect()
}

#[cfg(test)]
#[path = "tolerant_tests.rs"]
mod tests;
