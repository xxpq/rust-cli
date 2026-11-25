//! 插件管理的命令与写操作（docs/specs/rust-cli/protocol-v5.md §72.3、§72.4；docs/zcode-protocol-v5/24-plugins.md §4、§6）。
//!
//! ```text
//! 命令到（参数严格校验过；新 commandId）── 操作表 += {waiting}，排进到达顺序 ── 排队（§72.8）
//!   排队：比它早到、还没到终态的写里没有和它冲突的 → 置 running、起它自己的任务：PluginAdmin::apply(op, 取消令牌) → Done
//!   Done ── 重读（plugins.rs）── 进终态（发帧）── ACK ── 再排一次队
//! cancel：还没起任务（waiting）→ 当场终态 cancelled、不调端口；已起 → 触发令牌、cancelling，端口在提交点之前停下
//! ```
//!
//! 冲突按「碰到的东西」（24 §6.1）：同一个插件、都碰安装集合、都碰市场。不冲突的写同时进端口，锁内的提交由端口的实现排队
//! （plugin-store.md §4 的门与存储锁）。actor 自己起任务，所以起没起是 actor 的同步事实，取消不用和写任务抢。

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, OnceLock};

use rust_cli_protocol_core::schema::Node;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::commands::{invalid_payload, rejected, workspace_key};
use crate::consts::PLUGIN_OPERATIONS_RETAINED;
use crate::generated::catalog_plugins_v1::{
    CommandPluginsCancel, CommandPluginsConfigure, CommandPluginsConfigureScope,
    CommandPluginsSetEnabled, CommandResultPluginsCancel, CommandResultPluginsConfigure,
    PluginOperationFields, PluginOperationFieldsError, PluginOperationFieldsKind,
    PluginOperationFieldsPhase, PluginOperationFieldsScope,
};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandResultAccepted, CommandResultAcceptedEffectsItem,
    CommandResultDuplicate,
};
use crate::plugin_admin::{PluginAdmin, PluginApplied, PluginOp, PluginRefusal};
use crate::plugins::{OPERATIONS, PluginEvent, Then, fields};
use crate::publisher::{Batch, Fields};
use crate::server::{Actor, Event};
use crate::topics::PLUGINS_TOPIC;

const SCHEMA: &str = include_str!("../schema/catalog.plugins@1.json");
const COMMAND_PREFIX: &str = "Command.";
const DESCRIBE_PARAMS: &str = "Query.plugins.describe.params";

const INSTALL: &str = "plugins.install";
const UNINSTALL: &str = "plugins.uninstall";
const UPDATE: &str = "plugins.update";
const SET_ENABLED: &str = "plugins.setEnabled";
const CONFIGURE: &str = "plugins.configure";
const MARKETPLACE_ADD: &str = "plugins.marketplace.add";
const MARKETPLACE_REMOVE: &str = "plugins.marketplace.remove";
const CANCEL: &str = "plugins.cancel";
const RESCAN: &str = "plugins.rescan";

/// 编译好的命令参数与查询参数的 schema（给 rust 的 `schema/catalog.plugins@1.json`；`plugins_tests` 证明都能编译）。
pub(crate) struct Schemas {
    pub(crate) commands: BTreeMap<String, Node>,
    pub(crate) describe: Option<Node>,
}

pub(crate) fn schemas() -> &'static Schemas {
    static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let file: Value = serde_json::from_str(SCHEMA).unwrap_or(Value::Null);
        let named = file.get("schemas").and_then(Value::as_object);
        let compile = |schema: &Value| Node::compile(schema).ok();
        Schemas {
            commands: named
                .into_iter()
                .flatten()
                .filter_map(|(name, schema)| {
                    Some((
                        name.strip_prefix(COMMAND_PREFIX)?.to_owned(),
                        compile(schema)?,
                    ))
                })
                .collect(),
            describe: named
                .and_then(|named| named.get(DESCRIBE_PARAMS))
                .and_then(compile),
        }
    })
}

pub(crate) fn is_plugin_command(kind: &str) -> bool {
    schemas().commands.contains_key(kind)
}

/// 拒绝的码、字段与说明（24 §4.1）。
pub(crate) fn refusal(refused: &PluginRefusal) -> (&'static str, Option<String>, String) {
    match refused {
        PluginRefusal::PluginNotFound(id) => {
            (codes::PRECONDITION_PLUGIN_NOT_FOUND, None, id.clone())
        }
        PluginRefusal::MarketplaceNotFound(name) => (
            codes::PRECONDITION_MARKETPLACE_NOT_FOUND,
            None,
            name.clone(),
        ),
        PluginRefusal::OptionInvalid { key, message } => (
            codes::PRECONDITION_PLUGIN_OPTION_INVALID,
            Some(key.clone()),
            message.clone(),
        ),
        PluginRefusal::Cancelled => (
            codes::PRECONDITION_PLUGIN_OPERATION_CANCELLED,
            None,
            "cancelled before the change was committed".to_owned(),
        ),
        PluginRefusal::Failed(message) => (
            codes::PRECONDITION_PLUGIN_OPERATION_FAILED,
            None,
            message.clone(),
        ),
    }
}

/// 一条写碰到的东西（24 §6.1）：只看命令参数，不做 IO。
#[derive(Debug, Clone, Default)]
struct Footprint {
    plugin: Option<String>,
    /// 安装集合：装、更新、卸载（依赖闭包在下载之前不知道，卸载可能卸掉别人的依赖）。
    installs: bool,
    /// 市场：市场的三条，与要读市场清单的装、更新。
    markets: bool,
}

impl Footprint {
    fn of(op: &PluginOp) -> Self {
        let plugin = |id: &String| Some(id.clone());
        match op {
            PluginOp::Install(change) => Self {
                plugin: plugin(&change.plugin_id),
                installs: true,
                markets: true,
            },
            PluginOp::Update(change) => Self {
                plugin: plugin(&change.plugin_id),
                installs: true,
                markets: true,
            },
            PluginOp::Uninstall(change) => Self {
                plugin: plugin(&change.plugin_id),
                installs: true,
                markets: false,
            },
            PluginOp::SetEnabled { change, .. } => Self {
                plugin: plugin(&change.plugin_id),
                ..Self::default()
            },
            PluginOp::Configure { change, .. } => Self {
                plugin: plugin(&change.plugin_id),
                ..Self::default()
            },
            PluginOp::MarketplaceAdd(_)
            | PluginOp::MarketplaceRemove(_)
            | PluginOp::MarketplaceUpdate(_) => Self {
                markets: true,
                ..Self::default()
            },
        }
    }

    fn conflicts(&self, other: &Self) -> bool {
        (self.plugin.is_some() && self.plugin == other.plugin)
            || (self.installs && other.installs)
            || (self.markets && other.markets)
    }
}

/// 操作表里的一项。
struct Operation {
    fields: PluginOperationFields,
    footprint: Footprint,
    /// 还没起任务时的写与做它的端口；起任务时取走（`None` = 已经在跑或已到终态）。
    pending: Option<(PluginOp, Arc<dyn PluginAdmin>)>,
    cancel: CancellationToken,
    /// 原命令的 JSON-RPC id（终态时回 ACK）。
    reply: Option<Value>,
    /// 同一个 commandId 又来的请求：终态时回 `duplicate`。
    duplicates: Vec<Value>,
    /// 端口的结论；有它 = 已到终态。ACK 在回的那一刻按它拼（effects 取当时的位置，01 §7.3 的 G1）。
    verdict: Option<Verdict>,
}

type Verdict = Result<PluginApplied, PluginRefusal>;

/// 写操作：操作表、没到终态的到达顺序（排队按它）、终态的先后（保留按它删）。
#[derive(Default)]
pub(crate) struct Operations {
    table: BTreeMap<String, Operation>,
    open: Vec<String>,
    ended: VecDeque<String>,
}

impl Operations {
    /// 有没到终态的操作（`shutdown` 的回复等它们）。
    pub(crate) fn busy(&self) -> bool {
        self.table.values().any(|op| op.verdict.is_none())
    }

    /// `operations` 集合的内容（用户级 topic 登记时）。
    pub(crate) fn entities(&self) -> BTreeMap<String, Fields> {
        self.table
            .iter()
            .map(|(id, op)| (id.clone(), fields(&op.fields)))
            .collect()
    }
}

/// 一条写操作的端口结论：重读之后进终态（plugins.rs 的 `Then::Settle`）。
pub(crate) struct Settled {
    pub(crate) command_id: String,
    pub(crate) result: Verdict,
}

fn parse<T: DeserializeOwned>(payload: &Value) -> Result<T, Box<CommandResult>> {
    serde_json::from_value(payload.clone()).map_err(|error| {
        Box::new(rejected(
            codes::PRECONDITION_INVALID_PAYLOAD,
            Some(String::new()),
            Some(error.to_string()),
        ))
    })
}

fn accepted(
    effects: Vec<CommandResultAcceptedEffectsItem>,
    noop: bool,
    result: Value,
) -> CommandResult {
    CommandResult::Accepted(CommandResultAccepted {
        effects,
        noop,
        result: Some(result),
    })
}

fn duplicate(original: &CommandResult) -> CommandResult {
    CommandResult::Duplicate(CommandResultDuplicate {
        original: serde_json::to_value(original).unwrap_or(Value::Null),
    })
}

fn scope(scope: CommandPluginsConfigureScope) -> PluginOperationFieldsScope {
    match scope {
        CommandPluginsConfigureScope::User => PluginOperationFieldsScope::User,
        CommandPluginsConfigureScope::Workspace => PluginOperationFieldsScope::Workspace,
    }
}

/// 操作表里这条操作的事实（24 §3.5）：不带工作区（身份只在 topic 名里）。
fn operation_fields(op: &PluginOp) -> PluginOperationFields {
    use PluginOperationFieldsKind as Kind;
    let (kind, plugin_id, marketplace, scope) = match op {
        PluginOp::Install(change) => (Kind::Install, Some(&change.plugin_id), None, None),
        PluginOp::Uninstall(change) => (Kind::Uninstall, Some(&change.plugin_id), None, None),
        PluginOp::Update(change) => (Kind::Update, Some(&change.plugin_id), None, None),
        PluginOp::SetEnabled { change, .. } => (
            Kind::SetEnabled,
            Some(&change.plugin_id),
            None,
            Some(scope(change.scope)),
        ),
        PluginOp::Configure { change, .. } => (
            Kind::Configure,
            Some(&change.plugin_id),
            None,
            Some(scope(change.scope)),
        ),
        PluginOp::MarketplaceAdd(_) => (Kind::MarketplaceAdd, None, None, None),
        PluginOp::MarketplaceRemove(change) => (
            Kind::MarketplaceRemove,
            None,
            Some(&change.marketplace),
            None,
        ),
        PluginOp::MarketplaceUpdate(change) => (
            Kind::MarketplaceUpdate,
            None,
            change.marketplace.as_ref(),
            None,
        ),
    };
    PluginOperationFields {
        kind,
        plugin_id: plugin_id.cloned(),
        marketplace: marketplace.cloned(),
        scope,
        phase: PluginOperationFieldsPhase::Waiting,
        error: None,
    }
}

/// `scope: workspace` 的启停与配置要 `target.workspaceKey`（同 `agent.release` 的校验）；用户层不看它。
fn layer(
    command: &CommandEnvelope,
    scope: CommandPluginsConfigureScope,
) -> Result<Option<String>, Box<CommandResult>> {
    match scope {
        CommandPluginsConfigureScope::Workspace => workspace_key(command).map(Some),
        CommandPluginsConfigureScope::User => Ok(None),
    }
}

/// 写命令 → 写操作。`kind` 是 8 条写命令之一（cancel 与 rescan 在调用方先分出去了）。
fn operation(command: &CommandEnvelope) -> Result<PluginOp, Box<CommandResult>> {
    let payload = &command.payload;
    Ok(match command.r#type.as_str() {
        INSTALL => PluginOp::Install(parse(payload)?),
        UNINSTALL => PluginOp::Uninstall(parse(payload)?),
        UPDATE => PluginOp::Update(parse(payload)?),
        SET_ENABLED => {
            let change: CommandPluginsSetEnabled = parse(payload)?;
            let workspace = layer(command, change.scope)?;
            PluginOp::SetEnabled { workspace, change }
        }
        CONFIGURE => {
            let change: CommandPluginsConfigure = parse(payload)?;
            let workspace = layer(command, change.scope)?;
            PluginOp::Configure { workspace, change }
        }
        MARKETPLACE_ADD => PluginOp::MarketplaceAdd(parse(payload)?),
        MARKETPLACE_REMOVE => PluginOp::MarketplaceRemove(parse(payload)?),
        // 8 条里剩下的只有 `plugins.marketplace.update`
        _ => PluginOp::MarketplaceUpdate(parse(payload)?),
    })
}

impl Actor {
    /// `plugins.*` 命令（§72.3）：当场有结果的排 ACK；写操作的 ACK 等它的终态。分派处已确认有端口。
    pub(crate) fn plugin_command(
        &mut self,
        admin: Arc<dyn PluginAdmin>,
        id: &Value,
        command: &CommandEnvelope,
    ) {
        if let Some(result) = self.plugin_command_now(admin, id, command) {
            let value = serde_json::to_value(result).unwrap_or(Value::Null);
            self.outbox.reply(id.clone(), Ok(value));
        }
    }

    fn plugin_command_now(
        &mut self,
        admin: Arc<dyn PluginAdmin>,
        id: &Value,
        command: &CommandEnvelope,
    ) -> Option<CommandResult> {
        // 命令参数严格：先按 schema 校验（给出字段路径），再读成严格类型
        if let Some(schema) = schemas().commands.get(command.r#type.as_str())
            && let Err(error) = schema.validate(&command.payload, "")
        {
            return Some(invalid_payload(&error));
        }
        match command.r#type.as_str() {
            CANCEL => Some(match parse::<CommandPluginsCancel>(&command.payload) {
                Ok(cancel) => self.cancel_operation(&cancel.command_id),
                Err(result) => *result,
            }),
            RESCAN => {
                self.reread_plugins(admin, Then::Rescan(id.clone()));
                None
            }
            _ => self.write(admin, id, command),
        }
    }

    /// 一条写命令：重复的 commandId 回 `duplicate`（没到终态的等终态）；新的进操作表、排队（§72.8）。
    fn write(
        &mut self,
        admin: Arc<dyn PluginAdmin>,
        id: &Value,
        command: &CommandEnvelope,
    ) -> Option<CommandResult> {
        if let Some(op) = self.plugins.ops.table.get_mut(&command.command_id) {
            let Some(verdict) = op.verdict.clone() else {
                op.duplicates.push(id.clone());
                return None;
            };
            return Some(duplicate(&self.verdict_result(&verdict)));
        }
        let op = match operation(command) {
            Ok(op) => op,
            Err(result) => return Some(*result),
        };
        let command_id = command.command_id.clone();
        self.plugins.ops.table.insert(
            command_id.clone(),
            Operation {
                fields: operation_fields(&op),
                footprint: Footprint::of(&op),
                pending: Some((op, admin)),
                cancel: CancellationToken::new(),
                reply: Some(id.clone()),
                duplicates: Vec::new(),
                verdict: None,
            },
        );
        self.plugins.ops.open.push(command_id.clone());
        // 没有比它早到的冲突写就当场开始（起任务时发 `running`）；要等的才以 `waiting` 出现（24 §6.1）
        self.pump_plugin_writes();
        if self.plugins.ops.table[&command_id].pending.is_some() {
            self.publish_operations(&[&command_id]);
        }
        None
    }

    /// 排队（24 §6.1）：按到达顺序看还没起任务的写，比它早到、没到终态的写里没有和它冲突的就起它。同步的一步，没有 `.await`。
    fn pump_plugin_writes(&mut self) {
        let ops = &self.plugins.ops;
        let ready: Vec<String> = ops
            .open
            .iter()
            .enumerate()
            .filter(|(index, id)| {
                let op = &ops.table[*id];
                op.pending.is_some()
                    && !ops.open[..*index]
                        .iter()
                        .any(|earlier| ops.table[earlier].footprint.conflicts(&op.footprint))
            })
            .map(|(_, id)| id.clone())
            .collect();
        for command_id in ready {
            self.start_plugin_write(command_id);
        }
    }

    /// 起一条写的任务：置 `running`，端口做完发 `Done` 回来。
    fn start_plugin_write(&mut self, command_id: String) {
        let Some(op) = self.plugins.ops.table.get_mut(&command_id) else {
            return;
        };
        let Some((change, admin)) = op.pending.take() else {
            return;
        };
        op.fields.phase = PluginOperationFieldsPhase::Running;
        let cancel = op.cancel.clone();
        self.publish_operations(&[&command_id]);
        let events = self.events.clone();
        drop(self.spawner.spawn("protocol-v5-plugin-write", async move {
            let result = admin.apply(change, cancel).await;
            let _ = events.send(Event::Plugins(Box::new(PluginEvent::Done {
                command_id,
                result,
                admin,
            })));
        }));
    }

    /// `plugins.cancel`（24 §6）：`cancelled` = 发出了取消；最终是否取消掉看目标的终态。
    fn cancel_operation(&mut self, target: &str) -> CommandResult {
        let cancelled = self.cancel_one(target);
        let result = CommandResultPluginsCancel { cancelled };
        accepted(
            self.plugin_effects(),
            !cancelled,
            serde_json::to_value(result).unwrap_or(Value::Null),
        )
    }

    /// 取消一条没到终态的操作：还没开始的当场终态；在跑的触发令牌、置 `cancelling`。返回是否发出了取消。
    fn cancel_one(&mut self, target: &str) -> bool {
        let Some(op) = self.plugins.ops.table.get_mut(target) else {
            return false;
        };
        if op.verdict.is_some() {
            return false;
        }
        if op.pending.take().is_some() {
            // 还没起任务：不调端口，当场终态
            self.settle_operation(Settled {
                command_id: target.to_owned(),
                result: Err(PluginRefusal::Cancelled),
            });
        } else {
            // 已在跑：端口在提交点之前停下，或照常做完（结果看终态）
            op.cancel.cancel();
            op.fields.phase = PluginOperationFieldsPhase::Cancelling;
            self.publish_operations(&[target]);
        }
        true
    }

    /// `shutdown`（§72.4）：取消全部没到终态的操作，回复等它们（`Plugins::busy`）。后到的先取消：一条进终态时会再排一次队，
    /// 排在它后面的那时已经取消了，不会因此被起任务。
    pub(crate) fn cancel_plugin_operations(&mut self) {
        let open: Vec<String> = self.plugins.ops.open.iter().rev().cloned().collect();
        for id in open {
            self.cancel_one(&id);
        }
    }

    /// 写操作进终态：先发帧，再排 ACK（G2）；终态多于保留条数时删最早结束的那一条；之后再排一次队（等它的写可能能开始了）。
    pub(crate) fn settle_operation(&mut self, Settled { command_id, result }: Settled) {
        // 操作只在到了终态之后才会被挤出操作表：还没到终态的总在表里
        let Some(op) = self.plugins.ops.table.get_mut(&command_id) else {
            return;
        };
        if let Err(refused) = &result {
            let (code, _, message) = refusal(refused);
            op.fields.error = Some(PluginOperationFieldsError {
                code: code.to_owned(),
                message,
            });
        }
        op.fields.phase = match &result {
            Ok(_) => PluginOperationFieldsPhase::Succeeded,
            Err(PluginRefusal::Cancelled) => PluginOperationFieldsPhase::Cancelled,
            Err(_) => PluginOperationFieldsPhase::Failed,
        };
        let replies: Vec<(Value, bool)> = op
            .reply
            .take()
            .map(|id| (id, false))
            .into_iter()
            .chain(op.duplicates.drain(..).map(|id| (id, true)))
            .collect();
        op.verdict = Some(result.clone());
        let ops = &mut self.plugins.ops;
        ops.open.retain(|open| *open != command_id);
        ops.ended.push_back(command_id.clone());
        let mut touched = vec![command_id];
        while ops.ended.len() > PLUGIN_OPERATIONS_RETAINED {
            if let Some(oldest) = ops.ended.pop_front() {
                ops.table.remove(&oldest);
                touched.push(oldest);
            }
        }
        let touched: Vec<&str> = touched.iter().map(String::as_str).collect();
        self.publish_operations(&touched);
        let outcome = self.verdict_result(&result);
        for (id, again) in replies {
            let reply = if again {
                duplicate(&outcome)
            } else {
                outcome.clone()
            };
            let value = serde_json::to_value(reply).unwrap_or(Value::Null);
            self.outbox.reply(id, Ok(value));
        }
        self.pump_plugin_writes();
    }

    /// 结论 → ACK：成功是 `accepted`（effects 取此刻的位置），失败与取消是 `rejected`（24 §4.1）。
    fn verdict_result(&self, verdict: &Verdict) -> CommandResult {
        match verdict {
            Ok(applied) => accepted(
                self.plugin_effects(),
                applied.noop,
                serde_json::to_value(CommandResultPluginsConfigure {
                    diagnostics: applied.diagnostics.clone(),
                })
                .unwrap_or(Value::Null),
            ),
            Err(refused) => {
                let (code, field, message) = refusal(refused);
                rejected(code, field, Some(message))
            }
        }
    }

    /// `plugins.rescan` 读完了：ACK（effects 是当前位置）。
    pub(crate) fn rescanned(&mut self, reply: Value) {
        let result = accepted(
            self.plugin_effects(),
            false,
            Value::Object(Default::default()),
        );
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        self.outbox.reply(reply, Ok(value));
    }

    /// 操作表的这几项发进用户级 topic（登记了的话）：在表里的 put，不在的（被保留条数挤掉的）remove。一批一个 seq。
    fn publish_operations(&mut self, ids: &[&str]) {
        let Some(publisher) = self.topics.get_mut(PLUGINS_TOPIC) else {
            return;
        };
        let mut batch = Batch::default();
        for id in ids {
            match self.plugins.ops.table.get(*id) {
                Some(op) => batch.put(OPERATIONS, id, fields(&op.fields)),
                None => batch.remove(OPERATIONS, id),
            }
        }
        if publisher.publish(batch) {
            self.outbox.mark_topic(PLUGINS_TOPIC);
        }
    }
}
