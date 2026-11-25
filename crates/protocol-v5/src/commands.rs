//! 订阅与命令（01 §5.2、§7.3；protocol-v5.md §7、§8）。工作区级的命令：`agent.release`（01 §17.1：关掉一个工作区的内核）
//! 与 `mcp.trust.set`（18 §12.5，在 `mcp_trust.rs`）。它们都有效果，ACK 排在效果帧之后（G2）；设值语义，结果已是现状时
//! `accepted{noop}`，effects 是当前位置（G1：回放给出的总是当前 cursor）。命令参数严格（11 §5）：先按给 rust 的 schema 校验
//! （未知字段、未知枚举值 → 带字段路径的 rejected），再读成生成的严格类型。

use std::sync::OnceLock;

use rust_cli_protocol_core::schema::Node;
use serde_json::Value;

use crate::account_overlay::ACCOUNT_OVERLAY_SET;
use crate::errors::{Fault, not_applied, protocol_error};
use crate::generated::agent_v1::CommandAgentRelease;
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandResultAccepted, CommandResultAcceptedEffectsItem,
    CommandResultRejected, Subscribe,
};
use crate::hook_pretrust::HOOKS_TRUST_GRANT;
use crate::mcp_trust::MCP_TRUST_SET;
use crate::server::{Actor, decode};
use crate::topics::{self, Topic, TopicError, WORKSPACE_AGENT, workspace_topic};

/// `agent.release` 的命令类型（agent@1）。
pub(crate) const AGENT_RELEASE: &str = "agent.release";
/// 给 rust 的 agent@1 schema（生成物）里 `agent.release` 参数的 schema 名。
const AGENT_RELEASE_SCHEMA: &str = "Command.agent.release";
const AGENT_SCHEMA: &str = include_str!("../schema/agent@1.json");

/// 编译好的命令参数 schema：`file` 是给 rust 的一份 schema（生成物在编译期带进来；测试证明它能编译），编译一次存进 `cell`。
pub(crate) fn command_schema(
    cell: &'static OnceLock<Option<Node>>,
    file: &str,
    name: &str,
) -> Option<&'static Node> {
    cell.get_or_init(|| {
        let file: Value = serde_json::from_str(file).ok()?;
        Node::compile(file.get("schemas")?.get(name)?).ok()
    })
    .as_ref()
}

fn release_schema() -> Option<&'static Node> {
    static NODE: OnceLock<Option<Node>> = OnceLock::new();
    command_schema(&NODE, AGENT_SCHEMA, AGENT_RELEASE_SCHEMA)
}

pub(crate) fn rejected(
    code: &str,
    field: Option<String>,
    message: Option<String>,
) -> CommandResult {
    CommandResult::Rejected(CommandResultRejected {
        code: code.to_owned(),
        current: None,
        allowed: None,
        field,
        message,
    })
}

/// 校验器（protocol-core `schema.rs`）报“对象上的某个键”时的文字：键接在对象的路径后面。
const KEYED_MESSAGES: &[&str] = &["unrecognized property ", "missing required property "];
/// 校验器在根上的路径写法。
const ROOT_PATH: &str = "(root)";

/// 校验器的错误是 `/a/b: message`：取字段路径（点分，与 TS 的 decodeCommandPayload 相同；多余或缺少的键接在后面）。
/// 查询参数（`sessions.facts`）也用它。
pub(crate) fn field_path(error: &str) -> (String, &str) {
    let (path, message) = error.split_once(": ").unwrap_or(("", error));
    let mut parts: Vec<&str> = if path == ROOT_PATH {
        Vec::new()
    } else {
        path.split('/').filter(|part| !part.is_empty()).collect()
    };
    if let Some(key) = KEYED_MESSAGES
        .iter()
        .find_map(|prefix| message.strip_prefix(prefix))
    {
        parts.push(key);
    }
    (parts.join("."), message)
}

pub(crate) fn invalid_payload(error: &str) -> CommandResult {
    let (field, message) = field_path(error);
    rejected(
        codes::PRECONDITION_INVALID_PAYLOAD,
        Some(field),
        Some(message.to_owned()),
    )
}

impl Actor {
    pub(crate) fn subscribe(&mut self, id: &Value, params: Value) -> Result<(), Fault> {
        let subscribe: Subscribe = decode("subscribe", params)?;
        // 同一个 sub 改订别处：旧的 topic 少了一个订阅，订完之后对它复查（会话投影的释放，protocol-v5.md §66.0 第 4 条）
        let previous = self
            .outbox
            .topic_of(&subscribe.sub)
            .filter(|topic| *topic != subscribe.topic)
            .map(str::to_owned);
        let subscribed = self.subscribe_to(id, subscribe);
        if let Some(previous) = previous {
            self.left_topic(&previous);
        }
        subscribed
    }

    fn subscribe_to(&mut self, id: &Value, subscribe: Subscribe) -> Result<(), Fault> {
        match topics::parse(&subscribe.topic) {
            Ok(Topic::Agent) => {}
            // 插件管理（§72）：只在入口交了端口时提供；否则同不提供的 topic
            Ok(Topic::Plugins) if let Some(admin) = self.plugins.admin.clone() => {
                self.subscribe_plugins(admin, id, subscribe, None);
                return Ok(());
            }
            Ok(Topic::WorkspacePlugins { key }) if let Some(admin) = self.plugins.admin.clone() => {
                self.subscribe_plugins(admin, id, subscribe, Some(key));
                return Ok(());
            }
            Ok(Topic::Plugins | Topic::WorkspacePlugins { .. }) | Err(TopicError::NotFound) => {
                return Err(Box::new(protocol_error(
                    codes::PRECONDITION_NOT_FOUND,
                    format!("no topic {}", subscribe.topic),
                )));
            }
            Ok(Topic::WorkspaceAgent { key }) => self.ensure_workspace(&key),
            Ok(Topic::WorkspaceVerifications { key }) => self.ensure_index(&key),
            Ok(Topic::WorkspaceSessions { key }) => {
                self.subscribe_sessions(id, subscribe, &key);
                return Ok(());
            }
            Ok(Topic::WorkspaceSession { key, sid }) => {
                return self.subscribe_session(id, subscribe, &key, &sid);
            }
            Err(TopicError::Invalid(message)) => {
                return Err(Box::new(protocol_error(
                    codes::PRECONDITION_INVALID,
                    message,
                )));
            }
        }
        self.outbox.subscribe(
            id.clone(),
            &subscribe.sub,
            &subscribe.topic,
            subscribe.cursor,
        );
        Ok(())
    }

    /// 命令：结果排进出站队列（排在它的效果帧之后）。外壳不合法与不认识的类型直接回错误。
    pub(crate) fn command(&mut self, id: &Value, params: Value) -> Result<(), Fault> {
        let command: CommandEnvelope = decode("command", params)?;
        if crate::attachments::is_attachment_command(&command.r#type) {
            // 上传的暂存在 actor 里，提交在任务里；ACK 由它排进队列（protocol-v5.md §39.3）
            self.attachment_command(id, &command);
            return Ok(());
        }
        // 插件管理（§72）：只在入口交了端口时认它；写操作的 ACK 等终态，由 plugin_ops.rs 排进队列
        if crate::plugin_ops::is_plugin_command(&command.r#type)
            && let Some(admin) = self.plugins.admin.clone()
        {
            self.plugin_command(admin, id, &command);
            return Ok(());
        }
        // MCP 的授权（§74）：只在入口装了授权时认它；ACK 在任务做完之后
        if self.settings.mcp_oauth && crate::mcp_oauth::is_command(&command.r#type) {
            self.mcp_oauth_command(id, &command);
            return Ok(());
        }
        if crate::session::commands::is_session_command(&command.r#type) {
            // 会话命令在它的通道里执行，ACK 由通道的回执排进队列（protocol-v5.md §15.4）
            self.session_command(id, command);
            return Ok(());
        }
        let result = match command.r#type.as_str() {
            AGENT_RELEASE => self.release(&command),
            // 进程级：同步写格子，ACK 当场排出（account_overlay.rs）
            ACCOUNT_OVERLAY_SET => self.set_account_overlay(&command),
            // 写入交给工作任务，ACK 在它做完之后（mcp_trust.rs）
            MCP_TRUST_SET => match self.trust_mcp(id, &command) {
                Some(result) => result,
                None => return Ok(()),
            },
            // 同上（hook_pretrust.rs，与 mcp.trust.set 共用一个工作任务）
            HOOKS_TRUST_GRANT => match self.pretrust_hooks(id, &command) {
                Some(result) => result,
                None => return Ok(()),
            },
            // 同上（deletion.rs，Host 专用：归档会话的永久删除，protocol-v5.md §65）
            crate::deletion::DELETION_EXECUTE => match self.delete_permanently(id, &command) {
                Some(result) => result,
                None => return Ok(()),
            },
            other => {
                return Err(Box::new(not_applied(protocol_error(
                    codes::QUERY_UNSUPPORTED,
                    format!("unknown command {other}"),
                ))));
            }
        };
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        self.outbox.reply(id.clone(), Ok(value));
        Ok(())
    }

    fn release(&mut self, command: &CommandEnvelope) -> CommandResult {
        let key = match workspace_key(command) {
            Ok(key) => key,
            Err(result) => return *result,
        };
        // 命令参数严格：先校验（给出字段路径），再读成严格类型
        if let Some(schema) = release_schema()
            && let Err(error) = schema.validate(&command.payload, "")
        {
            return invalid_payload(&error);
        }
        if let Err(error) = serde_json::from_value::<CommandAgentRelease>(command.payload.clone()) {
            return rejected(
                codes::PRECONDITION_INVALID_PAYLOAD,
                Some(String::new()),
                Some(error.to_string()),
            );
        }
        let topic = workspace_topic(&key, WORKSPACE_AGENT);
        let applied = self.release_workspace(&key);
        let effects = self
            .topics
            .get(&topic)
            .map(|publisher| {
                let cursor = publisher.cursor();
                vec![CommandResultAcceptedEffectsItem {
                    topic: topic.clone(),
                    epoch: cursor.epoch,
                    seq: cursor.seq,
                }]
            })
            .unwrap_or_default();
        CommandResult::Accepted(CommandResultAccepted {
            effects,
            noop: !applied,
            result: None,
        })
    }
}

/// 工作区级命令的目标：`target.workspaceKey` 必填（缺 → `precondition.workspaceRequired`），且是这一端认的工作区（同 topic 的
/// 工作区段：绝对路径，否则 `precondition.invalid`）。
pub(crate) fn workspace_key(command: &CommandEnvelope) -> Result<String, Box<CommandResult>> {
    let Some(key) = command.target.workspace_key.clone() else {
        return Err(Box::new(rejected(
            codes::PRECONDITION_WORKSPACE_REQUIRED,
            Some("target.workspaceKey".to_owned()),
            None,
        )));
    };
    if let Err(error) = topics::parse(&workspace_topic(&key, WORKSPACE_AGENT)) {
        let message = match error {
            TopicError::Invalid(message) => message,
            TopicError::NotFound => format!("the workspace {key:?} is not valid"),
        };
        return Err(Box::new(rejected(
            codes::PRECONDITION_INVALID,
            Some("target.workspaceKey".to_owned()),
            Some(message),
        )));
    }
    Ok(key)
}
