//! 严格读（docs/zcode-protocol-v5/03-capabilities-handshake.md §3.8；与 `packages/protocol/src/codec/strict.ts` 的
//! `strictInbound` 同一规则）：initialize 带了契约指纹且相等的链路上，入站消息先按严格变体校验，再读成生成的类型。
//!
//! 严格变体从给 rust 的 schema 派生：每个带 `properties` 的对象不许多余的键。不认识的枚举值已经由 schema 里的 `enum`
//! 拒绝（降级值在 schema 之外，校验器看不到），所以只多这一条。契约里只有一份 schema（宽松的），不另生成一份。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use rust_cli_protocol_core::schema::Node;
use serde_json::{Map, Value};

use crate::generated::contract::METHOD_PARAMS;

const CORE_SCHEMA: &str = include_str!("../schema/core@1.json");

/// JSON Schema 里装子 schema 的关键字：严格变体要递归进去。
const NESTED_ONE: &[&str] = &["items", "additionalProperties"];
const NESTED_MANY: &[&str] = &["anyOf", "oneOf"];
const PROPERTIES: &str = "properties";
const ADDITIONAL: &str = "additionalProperties";

/// 严格变体：带 `properties` 又没写 `additionalProperties` 的对象加 `additionalProperties: false`；record（只有
/// `additionalProperties: S`）与不透明位置（`{}`）不变，只是递归进去。
pub fn strict_variant(schema: &Value) -> Value {
    let Value::Object(node) = schema else {
        return schema.clone();
    };
    let mut out = Map::new();
    for (key, value) in node {
        let nested = match key.as_str() {
            PROPERTIES => match value {
                Value::Object(props) => Value::Object(
                    props
                        .iter()
                        .map(|(name, prop)| (name.clone(), strict_variant(prop)))
                        .collect(),
                ),
                other => other.clone(),
            },
            k if NESTED_ONE.contains(&k) => strict_variant(value),
            k if NESTED_MANY.contains(&k) => match value {
                Value::Array(branches) => {
                    Value::Array(branches.iter().map(strict_variant).collect())
                }
                other => other.clone(),
            },
            _ => value.clone(),
        };
        out.insert(key.clone(), nested);
    }
    if out.contains_key(PROPERTIES) && !out.contains_key(ADDITIONAL) {
        out.insert(ADDITIONAL.to_owned(), Value::Bool(false));
    }
    Value::Object(out)
}

/// 每个 core 方法的参数的严格 schema（生成的 `METHOD_PARAMS` 给出名字；第一次用到时编译一次）。
fn compiled() -> &'static BTreeMap<&'static str, Node> {
    static SCHEMAS: OnceLock<BTreeMap<&'static str, Node>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let file: Value = serde_json::from_str(CORE_SCHEMA).unwrap_or(Value::Null);
        METHOD_PARAMS
            .iter()
            .filter_map(|(method, name)| {
                let node = Node::compile(&strict_variant(file.get("schemas")?.get(name)?)).ok()?;
                Some((*method, node))
            })
            .collect()
    })
}

pub(crate) fn params_schema(method: &str) -> Option<&'static Node> {
    compiled().get(method)
}

/// 按严格变体校验一条消息的参数；失败时给出校验器的说明（带 `/a/b` 路径）。没有这个方法的 schema 时不校验
/// （不认识的方法由分派处理）。
pub(crate) fn check(method: &str, params: &Value) -> Result<(), String> {
    match params_schema(method) {
        Some(schema) => schema.validate(params, ""),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "strict_tests.rs"]
mod tests;
