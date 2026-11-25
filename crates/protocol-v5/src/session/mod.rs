//! 会话领域（protocol-v5.md §15）：`ws/<key>/session/<sid>` 的投影与会话命令。连接的 actor 拿着每个会话的投影与 topic；
//! 每个会话一条通道（[`lane`]）串行地执行命令、把内核的事件流转回来。
//!
//! ```text
//! 订阅（topic 没登记）── pending ── 通道 Read ── Viewed ──► 建投影 → 登记 topic → 等着的订阅拿快照
//! 命令 ── 校验（UUIDv7、目标、参数、expect）── 通道 Run ── 内核 ── 事件 … Done ──► 取空会话列表的变化 → 排 ACK（效果之后）
//! ```

pub(crate) mod commands;
mod inputs;
pub(crate) use inputs::{credential, shown_attachment};
mod interactions;
pub(crate) mod lane;
mod live;
mod mcp_asks;
pub(crate) use live::LIVE;
mod debug;
pub(crate) mod extend;
mod fork;
mod preview;
mod projection;
mod prompts;
pub(crate) mod queries;
pub(crate) use queries::SessionQuery;
mod reads;
mod release;
mod resident;
mod retain;
mod rewind;
mod rows;
pub(crate) mod schemas;
mod side_chat;
mod slices;
mod state;
mod tool_output;
pub(crate) mod verify;

#[cfg(test)]
#[path = "load_tests.rs"]
mod load_tests;

use std::collections::{BTreeMap, BTreeSet};

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::KernelEvent;
use serde_json::Value;
use tokio::sync::mpsc;

use self::debug::DebugLog;
use self::lane::{LaneEvent, LaneParts, LaneRequest, Outcome, Ticket};
use self::projection::{Projection, residence};
use crate::consts::{SESSION_SNAPSHOT_BYTES, SESSION_SNAPSHOT_ROWS};
use crate::errors::{Fault, protocol_error, to_rpc};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResultAcceptedEffectsItem, Expect, Subscribe,
};
use crate::publisher::{Batch, Fields, Publisher};
use crate::server::Actor;
use crate::topics::{WORKSPACE_SESSIONS, session_topic, workspace_topic};

/// 一个会话在本连接上的状态。
struct Slot {
    key: String,
    topic: String,
    /// 通道的请求入口与它的代（只认当前一代的消息）。
    lane: Option<(mpsc::UnboundedSender<LaneRequest>, u64)>,
    projection: Option<Projection>,
    /// 已经让通道读视图、还没有回音（等着的订阅不重复读）。
    reading: bool,
    /// 开发者面板的调试记录（§43.2）：通道转来的内核事件喂它，投影重建不清（释放时随投影丢，§66）。
    debug: DebugLog,
    /// 交给通道、还没有结局的命令（§66.2 的在途协议请求）：`Done` 要这个表项与 topic，在途时不释放。
    commands: usize,
    /// 这个会话的投影在本进程里释放过几次：重建的 topic 用它拼新 epoch（§66.0 第 2 条）。
    releases: u64,
    /// 交给通道、还没回来的要运行时的读（§43.2）：在途时不回收运行时（§69.3）。
    runtime_reads: usize,
    /// 热会话回收里的事实（「用」的先后、在不在关，§69）。
    residence: resident::Residence,
    /// 冷投影的保留（当前投影带不带库里的事实、保留着的那份、等复核的订阅，§70）。
    retention: retain::Retention,
}

/// actor 里的会话领域。
#[derive(Default)]
pub(crate) struct Sessions {
    slots: BTreeMap<String, Slot>,
    generations: u64,
    /// 进程级部件建好之前就有人订阅的会话（工作区，会话）：建好时让通道读。
    waiting: BTreeSet<(String, String)>,
    /// 还没回执的会话命令（`shutdown` 的回复等它们都排进出站队列，03 §3.4）。
    pub(crate) in_flight: usize,
    /// 在途的读（没有物化的会话的只读查询、通道里的会话运行时的读，protocol-v5.md §40.3、§43.3）：`shutdown` 的回复也等它们。
    pub(crate) reads: usize,
    /// 临时投影的序号（每次读视图加一，进程内不复用）：拼成那份临时投影的 epoch。
    temporary: u64,
    /// 「用」的序号（会话从忙变闲一次加一，§69.0 第 2 条）：热会话回收按它排先后，不读时钟。
    uses: u64,
    /// 进保留的序号（一个冷投影进保留一次加一，§70.0 第 2 条）：挤出最久没用的按它，不读时钟。
    retained: u64,
}

impl Sessions {
    /// 这个会话所在的工作区（会话表里有它时）：CLI→Host 的请求按它定外壳的工作区（protocol-v5.md §30）。
    pub(crate) fn workspace_of(&self, sid: &str) -> Option<&str> {
        self.slots.get(sid).map(|slot| slot.key.as_str())
    }

    /// 这个会话的 `session.debug`（§43.2）：会话表里没有它时是空的记录。
    pub(crate) fn debug(&self, sid: &str) -> Value {
        self.slots
            .get(sid)
            .map(|slot| &slot.debug)
            .unwrap_or(&DebugLog::default())
            .snapshot(sid)
    }
}

impl Actor {
    /// 这个会话的通道（没有就起一条）。进程级部件还没建好时为 None。
    fn lane(&mut self, key: &str, sid: &str) -> Option<mpsc::UnboundedSender<LaneRequest>> {
        let (workspaces, queries) = self.sources()?;
        let slot = self
            .sessions
            .slots
            .entry(sid.to_owned())
            .or_insert_with(|| Slot {
                key: key.to_owned(),
                topic: session_topic(key, sid),
                lane: None,
                projection: None,
                reading: false,
                debug: DebugLog::default(),
                commands: 0,
                releases: 0,
                runtime_reads: 0,
                residence: resident::Residence::default(),
                retention: retain::Retention::default(),
            });
        if let Some((requests, _)) = &slot.lane {
            return Some(requests.clone());
        }
        self.sessions.generations += 1;
        let generation = self.sessions.generations;
        let parts = LaneParts {
            sid: SessionId::new(sid),
            key: key.to_owned(),
            generation,
            workspaces,
            queries,
            out: self.events.clone(),
        };
        let requests = lane::start(parts, &self.spawner);
        slot.lane = Some((requests.clone(), generation));
        Some(requests)
    }

    /// 这个会话已经落在别的工作区下（一个会话只属于一个工作区，topic 与命令按它定位）。
    fn other_workspace(&self, key: &str, sid: &str) -> bool {
        self.sessions
            .slots
            .get(sid)
            .is_some_and(|slot| slot.key != key)
    }

    /// 订阅 `ws/<key>/session/<sid>`（§15.2）：登记了照常；没有 → pending，让通道读视图（物化完再登记）。
    pub(crate) fn subscribe_session(
        &mut self,
        id: &Value,
        subscribe: Subscribe,
        key: &str,
        sid: &str,
    ) -> Result<(), Fault> {
        if self.other_workspace(key, sid) {
            return Err(Box::new(protocol_error(
                codes::PRECONDITION_WORKSPACE_MISMATCH,
                format!("the session {sid} belongs to another workspace"),
            )));
        }
        let topic = session_topic(key, sid);
        if self.topics.contains_key(&topic) {
            self.outbox
                .subscribe(id.clone(), &subscribe.sub, &topic, subscribe.cursor);
            return Ok(());
        }
        // 保留着的冷投影（§70）：先复核它还是不是库里的样子，再登记回去
        if self.subscribe_kept(id, &subscribe, key, sid) {
            return Ok(());
        }
        self.outbox
            .subscribe_pending(id.clone(), &subscribe.sub, &topic, subscribe.cursor);
        self.read(key, sid);
        Ok(())
    }

    /// 让通道读视图（同一个会话只读一次，回音之前不重复）。进程级部件没建好时记下，建好再读。
    fn read(&mut self, key: &str, sid: &str) {
        let Some(requests) = self.lane(key, sid) else {
            self.sessions
                .waiting
                .insert((key.to_owned(), sid.to_owned()));
            return;
        };
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        if !slot.reading && slot.projection.is_none() {
            slot.reading = true;
            let _ = requests.send(LaneRequest::Read);
        }
    }

    /// 进程级部件建好：读之前就有人订阅的会话。
    pub(crate) fn read_waiting(&mut self) {
        for (key, sid) in std::mem::take(&mut self.sessions.waiting) {
            self.read(&key, &sid);
        }
    }

    /// 一条会话命令（§15.4）：校验通过的交给通道，回执由通道的 `Done` 排进出站队列。
    pub(crate) fn session_command(&mut self, reply: &Value, envelope: CommandEnvelope) {
        let slot = envelope
            .target
            .session_id
            .as_ref()
            .and_then(|sid| self.sessions.slots.get(sid));
        let projection = slot.and_then(|slot| slot.projection.as_ref());
        // 前端持有的校验在 actor 里裁决（§35.2）：不经通道、不进内核
        match commands::front_answer(&envelope, projection) {
            Some(Ok(front)) => return self.answer_verification(reply, front),
            Some(Err(result)) => return self.reply_result(reply, &result),
            None => {}
        }
        let publisher = slot.and_then(|slot| self.topics.get(&slot.topic));
        let accepted = match commands::accept(&envelope, projection, publisher) {
            Ok(accepted) => accepted,
            Err(result) => return self.reply_result(reply, &result),
        };
        if self.other_workspace(&accepted.key, &accepted.sid) {
            let result = commands::rejected(
                codes::PRECONDITION_WORKSPACE_MISMATCH,
                Some("target.workspaceKey".to_owned()),
                None,
            );
            return self.reply_result(reply, &result);
        }
        let Some(requests) = self.lane(&accepted.key, &accepted.sid) else {
            let result = commands::rejected(
                codes::PRECONDITION_STORE_UNAVAILABLE,
                None,
                Some("the session store is not ready yet".to_owned()),
            );
            return self.reply_result(reply, &result);
        };
        let entity = match &envelope.expect {
            Some(Expect::Alt1(expect)) => Some(expect.id.clone()),
            _ => None,
        };
        let ticket = Ticket {
            reply: reply.clone(),
            id: envelope.command_id.clone(),
            kind: envelope.r#type.clone(),
            entity,
        };
        if requests
            .send(LaneRequest::Run {
                ticket,
                op: Box::new(accepted.op),
            })
            .is_ok()
        {
            self.sessions.in_flight += 1;
            if let Some(slot) = self.sessions.slots.get_mut(&accepted.sid) {
                slot.commands += 1;
                // 命令可能改库：投影不再是冷读那一份的样子（§70.0 第 2 条）
                slot.retention.touched();
            }
        }
    }

    /// 侧聊的首条输入交给侧聊的通道（§57.2）：在途的命令数不变，ACK 由侧聊的 `Done` 排。
    fn first_input(
        &mut self,
        parent: &str,
        child: &SessionId,
        ticket: Ticket,
        first: rust_cli_kernel_api::session_api::Command,
    ) {
        let key = self.sessions.slots.get(parent).map(|slot| slot.key.clone());
        let requests = key.and_then(|key| self.lane(&key, child.as_str()));
        let op = Box::new(commands::live(first));
        if let Some(requests) = requests
            && requests.send(LaneRequest::Run { ticket, op }).is_ok()
        {
            if let Some(slot) = self.sessions.slots.get_mut(child.as_str()) {
                slot.commands += 1;
                slot.retention.touched();
            }
            return;
        }
        // 通道的任务已经退出（只在进程关闭时；进程级部件父会话的通道就在用）：同 `session_command` 不回执，在途数减一，shutdown 不等它
        self.sessions.in_flight -= 1;
        self.finish_shutdown();
    }

    fn reply_result(&mut self, reply: &Value, result: &crate::generated::core_v1::CommandResult) {
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        self.outbox.reply(reply.clone(), Ok(value));
    }

    /// 通道来的一条消息（只认当前一代）。处理前后比一比热会话的事实：新激活、从忙变闲时复查上限（§69.0 第 5 条）。
    pub(crate) fn on_lane(&mut self, sid: &str, generation: u64, event: LaneEvent) {
        let Some(slot) = self.sessions.slots.get(sid) else {
            return;
        };
        if slot.lane.as_ref().map(|(_, current)| *current) != Some(generation) {
            return;
        }
        let before = resident::Seen::of(slot);
        self.lane_event(sid, event);
        self.resident_after(sid, before);
    }

    fn lane_event(&mut self, sid: &str, event: LaneEvent) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        match event {
            LaneEvent::Viewed { view, mut origin } => {
                // 订阅要的读视图回来时订阅已经走了、也没有命令在等（§66.4 第 1 行）：不建投影，丢掉视图与通道
                let read = std::mem::take(&mut slot.reading);
                if read && !origin.live && !release::held(slot, &self.outbox) {
                    self.release_unheld(sid);
                    return;
                }
                slot.debug.traced(view.meta.trace.as_str());
                // 新的视图（复核不同之后的冷读、命令打开了会话）：保留的那份过时了，丢掉，下面的 epoch 随之换新（§70）
                slot.discard_kept();
                let stamp = origin.stamp.take();
                // 前端持有的校验不在内核的视图里：从旧投影带过来（§35.2）
                let kept = slot.projection.as_mut().map(Projection::take_verifications);
                // 激活（冷 → 热）：沿用冷的投影的下沿与号，由活视图建的这一份与它逐项相同，不多发快照（§73.0 第 1 条）
                if origin.live
                    && let Some(old) = slot.projection.as_ref()
                {
                    origin.start = old.activation();
                }
                // 冷读只建窗口（§71.3）：窗口外不留整行的行建的时候就换成骨架；活视图照旧建全部整行（在跑的回合之后才定哪些要留）
                let (mut projection, mut content) = if origin.live {
                    Projection::build(&view, origin)
                } else {
                    Projection::build_window(&view, origin)
                };
                projection.keep_verifications(kept.unwrap_or_default(), &mut content);
                let topic = slot.topic.clone();
                // 释放过的会话重建时换 epoch（§66.0 第 2 条）：带旧 cursor 的订阅一定拿快照
                let epoch = release::topic_epoch(&self.epoch, slot.releases);
                let publisher = self.topics.entry(topic.clone()).or_insert_with(|| {
                    Publisher::new(topic.clone(), epoch)
                        .with_window(
                            rows::ROWS,
                            rows::CREATED_AT_SEQ,
                            SESSION_SNAPSHOT_ROWS,
                            SESSION_SNAPSHOT_BYTES,
                        )
                        .with_openings(rows::opens)
                });
                // 物化完再登记（01 §5.2）：新 topic 的第一份内容就是首帧；已有的（冷 → 热、改写历史）按 §15.3.6 换内容
                publisher.adopt(content);
                // 订阅中的会话按窗口驻留（§67）：窗口外不要整行、订阅都已收到的行换成骨架（§68.0 第 5 条）
                residence::thin(publisher, &projection, &self.outbox);
                slot.projection = Some(projection);
                slot.retention.built(stamp);
                self.outbox.mark_topic(&topic);
                // 复核期间命令的视图先登记了 topic：等着的订阅随登记回复
                self.answer_waiters(sid);
            }
            LaneEvent::Rebuilt(view) => {
                let Some(old) = slot.projection.as_mut() else {
                    return;
                };
                let (origin, kept) = (old.origin(), old.take_verifications());
                let (mut projection, mut content) = Projection::build(&view, origin);
                projection.keep_verifications(kept, &mut content);
                if let Some(publisher) = self.topics.get_mut(&slot.topic) {
                    publisher.adopt(content);
                    residence::thin(publisher, &projection, &self.outbox);
                    self.outbox.mark_topic(&slot.topic);
                }
                slot.projection = Some(projection);
            }
            LaneEvent::Kernel(event) => {
                slot.debug.observe(&event);
                if let (Some(projection), Some(publisher)) =
                    (slot.projection.as_mut(), self.topics.get_mut(&slot.topic))
                {
                    let batch = projection.apply(publisher, &event);
                    if publisher.publish(batch) {
                        self.outbox.mark_topic(&slot.topic);
                    }
                    // 回合结束：这一轮的行不再变，窗口外的换成骨架（§67.0 第 3 条）
                    if matches!(event.event, KernelEvent::TurnFinished { .. }) {
                        residence::thin(publisher, projection, &self.outbox);
                    }
                }
            }
            LaneEvent::Failed(rejection) => {
                slot.reading = false;
                // 复核读出拒绝（§70）：等复核的订阅先回 pending，下面与读视图失败一样收 subscription.failed
                self.check_refused(sid);
                let Some(slot) = self.sessions.slots.get(sid) else {
                    return;
                };
                if !self.topics.contains_key(&slot.topic) {
                    let error = protocol_error(rejection.code, rejection.message);
                    self.outbox.fail_topic(&slot.topic, &error);
                }
                // 读视图失败了，等它的订阅也都结束了（§66.0 第 4 条 ④）
                self.release_unheld(sid);
                self.finish_shutdown();
            }
            LaneEvent::Done {
                ticket,
                outcome,
                then,
            } => {
                slot.commands -= 1;
                // 侧聊的首条输入（protocol-v5.md §57.2）：父会话分出了侧聊（不是重放），接着在侧聊的通道里开第一轮（同一个 commandId），ACK 等它
                if let (Some(first), Outcome::Ack(ack)) = (then, &outcome)
                    && let Some(child) = side_chat::created(ack)
                {
                    self.first_input(sid, &child, ticket, *first);
                } else {
                    self.done(sid, ticket, outcome);
                }
                // 命令有了结局（§66.0 第 4 条 ③）：先排了 ACK，再复查
                self.release_unheld(sid);
                return;
            }
            LaneEvent::Closed { cold } => return self.closed(sid, cold),
            LaneEvent::Checked { same } => return self.checked(sid, same),
        }
        self.live_changed(sid);
    }

    /// 一个会话的活性变了（会话列表的 `live`，§14.1）：投影是唯一来源，回合在跑或有待回答的交互时有这一行，否则没有。会话列表
    /// 还没物化时不发（物化时从投影补，[`Self::live_rows`]）。值没变不发、不占 seq。
    fn live_changed(&mut self, sid: &str) {
        let Some(slot) = self.sessions.slots.get(sid) else {
            return;
        };
        let feed = workspace_topic(&slot.key, WORKSPACE_SESSIONS);
        if !self.topics.contains_key(&feed) {
            return;
        }
        let row = slot
            .projection
            .as_ref()
            .and_then(|projection| projection.hot(self.topics.get(&slot.topic)))
            .map(|hot| live::row(&hot));
        // 事实先于活性（04 §4.1）：Host 按 live 的移除判回合结束、读 facts 的 lastOutcome；落库的事实在内核发回合结束之前
        // 已经进了变化流，先取空它
        self.drain_fact_changes();
        let mut batch = Batch::default();
        match row {
            Some(fields) => batch.put(live::LIVE, sid, fields),
            None => batch.remove(live::LIVE, sid),
        }
        if let Some(publisher) = self.topics.get_mut(&feed)
            && publisher.publish(batch)
        {
            self.outbox.mark_topic(&feed);
        }
    }

    /// 物化会话列表时 `live` 的首帧（§14.4）：这个工作区里正在跑的会话，取自它们的投影。
    pub(crate) fn live_rows(&self, key: &str) -> Vec<(String, Fields)> {
        self.sessions
            .slots
            .iter()
            .filter(|(_, slot)| slot.key == key)
            .filter_map(|(sid, slot)| {
                let hot = slot
                    .projection
                    .as_ref()?
                    .hot(self.topics.get(&slot.topic))?;
                Some((sid.clone(), live::row(&hot)))
            })
            .collect()
    }

    /// 本进程写下 MCP 信任之后（18 §12.6）：这个工作区每个活会话换上写入之后的 `mcpAdmission`。返回改了的会话 topic 的 cursor
    /// （命令的 effects）；值没变的不发、不占 seq。
    pub(crate) fn mcp_admission_changed(
        &mut self,
        key: &str,
        value: &Value,
    ) -> Vec<CommandResultAcceptedEffectsItem> {
        let mut effects = Vec::new();
        for slot in self
            .sessions
            .slots
            .values_mut()
            .filter(|slot| slot.key == key)
        {
            let (Some(projection), Some(publisher)) =
                (slot.projection.as_mut(), self.topics.get_mut(&slot.topic))
            else {
                continue;
            };
            if publisher.publish(projection.mcp_admission(value)) {
                self.outbox.mark_topic(&slot.topic);
                let cursor = publisher.cursor();
                effects.push(CommandResultAcceptedEffectsItem {
                    topic: slot.topic.clone(),
                    epoch: cursor.epoch,
                    seq: cursor.seq,
                });
            }
        }
        effects
    }

    /// 会话被删了（`session.delete` 的回执；归档会话的永久删除，protocol-v5.md §65.1）：投影标成 deleted（交互清空、准入一律拒绝）；
    /// 它不再「正在跑」（§14.1），待校验的索引里也没有它（§35.2）。这个连接上没有它的会话表项时什么都不做。
    pub(crate) fn mark_deleted(&mut self, sid: &str) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        // 删除了的不进保留，保留着的丢掉（§70.4）
        slot.discard_kept();
        slot.retention.touched();
        let mut unindexed = Vec::new();
        if let (Some(projection), Some(publisher)) =
            (slot.projection.as_mut(), self.topics.get_mut(&slot.topic))
        {
            unindexed = projection.verification_ids();
            let batch = projection.deleted_now(publisher);
            if publisher.publish(batch) {
                self.outbox.mark_topic(&slot.topic);
            }
        }
        let key = slot.key.clone();
        self.unindex(&key, unindexed);
        self.live_changed(sid);
    }

    /// 一条命令的结局：先让它的效果都进 topic（会话列表的变化在另一条流里，取空它），再排 ACK（G2，§15.4.2）。
    fn done(&mut self, sid: &str, ticket: Ticket, outcome: Outcome) {
        self.drain_fact_changes();
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        match &outcome {
            Outcome::Renamed(title) => {
                if let (Some(projection), Some(publisher)) =
                    (slot.projection.as_mut(), self.topics.get_mut(&slot.topic))
                    && publisher.publish(projection.renamed(title))
                {
                    self.outbox.mark_topic(&slot.topic);
                }
            }
            Outcome::Deleted => self.mark_deleted(sid),
            _ => {}
        }
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        let result = commands::finish(
            &ticket,
            sid,
            outcome,
            slot.projection.as_ref(),
            self.topics.get(&slot.topic),
            &slot.topic,
        );
        let reply = match result {
            Ok(result) => Ok(serde_json::to_value(result).unwrap_or(Value::Null)),
            Err(rejection) => Err(to_rpc(&protocol_error(rejection.code, rejection.message))),
        };
        self.outbox.reply(ticket.reply, reply);
        self.sessions.in_flight -= 1;
        self.finish_shutdown();
    }
}
