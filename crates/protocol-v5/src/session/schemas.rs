//! 会话命令、应答与查询参数的 schema（给 rust 的 `schema/session@1.json`、`session.queue@1.json`，生成物在编译期带进来；`session_tests`
//! 证明它们都能编译）。命令参数严格（11 §5）：命令的 schema 本来就不许多余的键；应答在命令参数里是 `unknown`，按应答的
//! schema 的严格变体校验（03 §3.8 的 `strict_variant`），新枚举值与新字段都拒绝。查询参数按 schema 原样校验（宽松，11 §11 第 3 项）。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use rust_cli_protocol_core::schema::Node;
use serde_json::Value;

use crate::strict::strict_variant;

const SESSION_SCHEMA: &str = include_str!("../../schema/session@1.json");
const QUEUE_SCHEMA: &str = include_str!("../../schema/session.queue@1.json");
/// 改写历史（protocol-v5.md §53）：`turn.edit`、`turn.retry`、`files.rewind` 与预览查询的参数。
const REWIND_SCHEMA: &str = include_str!("../../schema/session.rewind@1.json");
/// fork（protocol-v5.md §55）：`turn.fork` 的参数。
const FORK_SCHEMA: &str = include_str!("../../schema/session.fork@1.json");
/// 会话命令 id 的 schema（UUIDv7，02 §6）。
const COMMAND_ID: &str = "SessionCommandIdSchema";
/// 命令参数与应答的 schema 名前缀。
const COMMAND_PREFIX: &str = "Command.";
const ANSWER_PREFIX: &str = "InteractionAnswer.";
/// `host.*` 服务的结果：`Service.<名字>.result`（protocol-v5.md §30）。
const SERVICE_PREFIX: &str = "Service.";
const RESULT_SUFFIX: &str = ".result";
/// 查询参数：`Query.<类型>.params`（protocol-v5.md §40.2）。
const QUERY_PREFIX: &str = "Query.";
const PARAMS_SUFFIX: &str = ".params";

struct Schemas {
    commands: BTreeMap<String, Node>,
    answers: BTreeMap<String, Node>,
    services: BTreeMap<String, Node>,
    queries: BTreeMap<String, Node>,
    command_id: Option<Node>,
}

fn schemas() -> &'static Schemas {
    static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let mut out = Schemas {
            commands: BTreeMap::new(),
            answers: BTreeMap::new(),
            services: BTreeMap::new(),
            queries: BTreeMap::new(),
            command_id: None,
        };
        for text in [SESSION_SCHEMA, QUEUE_SCHEMA, REWIND_SCHEMA, FORK_SCHEMA] {
            let file: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            let Some(Value::Object(named)) = file.get("schemas") else {
                continue;
            };
            for (name, schema) in named {
                if let Some(command) = name.strip_prefix(COMMAND_PREFIX) {
                    if let Ok(node) = Node::compile(schema) {
                        out.commands.insert(command.to_owned(), node);
                    }
                } else if let Some(kind) = name.strip_prefix(ANSWER_PREFIX) {
                    if let Ok(node) = Node::compile(&strict_variant(schema)) {
                        out.answers.insert(kind.to_owned(), node);
                    }
                } else if let Some(service) = name
                    .strip_prefix(SERVICE_PREFIX)
                    .and_then(|rest| rest.strip_suffix(RESULT_SUFFIX))
                {
                    // Host 的应答在严格链路上按严格变体校验（03 §3.8）
                    if let Ok(node) = Node::compile(&strict_variant(schema)) {
                        out.services.insert(service.to_owned(), node);
                    }
                } else if let Some(query) = name
                    .strip_prefix(QUERY_PREFIX)
                    .and_then(|rest| rest.strip_suffix(PARAMS_SUFFIX))
                {
                    if let Ok(node) = Node::compile(schema) {
                        out.queries.insert(query.to_owned(), node);
                    }
                } else if name == COMMAND_ID {
                    out.command_id = Node::compile(schema).ok();
                }
            }
        }
        out
    })
}

/// 命令参数的 schema（`Command.<type>`）。
pub(crate) fn command(kind: &str) -> Option<&'static Node> {
    schemas().commands.get(kind)
}

/// 应答的严格 schema（`InteractionAnswer.<kind>`）。
pub(crate) fn answer(kind: &str) -> Option<&'static Node> {
    schemas().answers.get(kind)
}

/// `host.*` 服务结果的严格 schema（`Service.<name>.result`）。
pub(crate) fn service_result(name: &str) -> Option<&'static Node> {
    schemas().services.get(name)
}

/// 查询参数的 schema（`Query.<type>.params`）。
pub(crate) fn query(kind: &str) -> Option<&'static Node> {
    schemas().queries.get(kind)
}

/// 会话命令的 id 是 UUIDv7（02 §6，§11.2 A）。
pub(crate) fn is_command_id(id: &str) -> bool {
    schemas()
        .command_id
        .as_ref()
        .is_some_and(|node| node.validate(&Value::from(id), "").is_ok())
}
