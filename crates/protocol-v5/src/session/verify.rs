//! 前端持有的 `requestVerification` 交互（docs/specs/rust-cli/protocol-v5.md §35；docs/zcode-protocol-v5/21-request-auth.md §13.2.2）：
//! 需要用户完成官方版本的请求安全校验时，取凭据的任务（`HostCredentials`）把一次校验交给连接的 actor；实体归这个会话的投影，
//! 不进内核。actor 在同一次处理里写会话 topic 的 `interactions`、工作区的待校验索引 `ws/<key>/verifications` 与会话列表的 `live`。
//!
//! ```text
//! 任务 ─ Event::Verify{会话, id, provider, challenge, expiresAt, 应答口} ─► actor：投影 put 实体 → 索引 put 行 → live 重算
//! 窗口 ─ interaction.resolve{expect{interactions, id}, answer{token | failure}} ─► actor：实体在 → 同一批移除、交给应答口、ACK accepted
//! 任务被丢掉（回合停了、到了截止时刻）─ Event::Withdraw{会话, id} ─► actor：实体在就移除（应答之后再撤回什么也不做）
//! ```
//!
//! 先到者胜：应答是命令、撤回是内部事件，都进 actor 的同一个循环，按到达处理（02 §7）。

use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::commands::rejected;
use super::interactions::{BadAnswer, INTERACTIONS};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandResult, CommandResultAccepted, CommandResultAcceptedEffectsItem,
};
use crate::generated::session_v1::InteractionAnswerRequestVerification;
use crate::publisher::{Batch, Fields, Publisher};
use crate::server::Actor;
use crate::topics::{WORKSPACE_VERIFICATIONS, workspace_topic};

/// 交互的种类（`INTERACTION_KINDS`）。
pub(crate) const REQUEST_VERIFICATION: &str = "requestVerification";
/// 待校验的索引 topic 的集合名（`ws/<key>/verifications`）。
pub(crate) const VERIFICATIONS: &str = "verifications";

/// 开不了交互的原因（给用户看：模型请求以鉴权失败结束）。
const NOT_OPEN: &str = "the requesting session is not open in this rust process";

/// 窗口给的应答：校验的结果（形状由 Host 定义，rust 原样交回），或校验没做成的说明。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VerifyAnswer {
    Token(String),
    Failure(String),
}

/// 应答口：actor 交回应答，或开不了交互的原因。被丢掉 = 交互没被应答就结束了（会话删除、链路断开、撤回）。
pub(crate) type AnswerPort = oneshot::Sender<Result<VerifyAnswer, String>>;

/// 取凭据的任务交给 actor 的一次校验。`id` 由任务生成（跨进程不重复），撤回按它。
pub(crate) struct Verify {
    pub(crate) session: String,
    pub(crate) id: String,
    pub(crate) provider: String,
    pub(crate) challenge: Value,
    /// 截止时刻（Unix 毫秒）：与任务的截止时刻同一个起点，只供显示。
    pub(crate) expires_at: u64,
    pub(crate) answer: AnswerPort,
}

/// 投影里一个待回答的校验：实体的字段与应答口。实体从 put 到 remove 不变。
#[derive(Debug)]
pub(crate) struct Pending {
    provider: String,
    challenge: Value,
    expires_at: u64,
    answer: AnswerPort,
}

impl Pending {
    /// 会话 topic 里 `interactions` 的实体（不含 id、v）。
    pub(crate) fn entity(&self) -> Fields {
        let mut fields = Fields::new();
        fields.insert("kind".to_owned(), json!(REQUEST_VERIFICATION));
        fields.insert(
            "payload".to_owned(),
            json!({"provider": self.provider, "challenge": self.challenge}),
        );
        fields.insert("expiresAt".to_owned(), json!(self.expires_at));
        fields
    }

    /// 索引 topic 的行（`RequestVerificationEntrySchema`，不含 id、v）。
    pub(crate) fn row(&self, sid: &str) -> Fields {
        let mut fields = Fields::new();
        fields.insert("sessionId".to_owned(), json!(sid));
        fields.insert("provider".to_owned(), json!(self.provider));
        fields.insert("challenge".to_owned(), self.challenge.clone());
        fields.insert("expiresAt".to_owned(), json!(self.expires_at));
        fields
    }

    fn give(self, answer: VerifyAnswer) {
        // 任务已经放弃（回合停了）时发不进去：应答没有效果（21 §13.4 第 10 行）
        let _ = self.answer.send(Ok(answer));
    }
}

/// 一个 `interaction.resolve` 落在前端持有的校验上：外壳与应答都校验过了。
pub(crate) struct FrontAnswer {
    pub(crate) key: String,
    pub(crate) sid: String,
    pub(crate) id: String,
    pub(crate) answer: VerifyAnswer,
}

/// 应答按严格 schema 校验（`token`、`failure` 恰好一个），再读成哪一种。
pub(crate) fn decode(answer: &Value) -> Result<VerifyAnswer, BadAnswer> {
    let decoded: InteractionAnswerRequestVerification =
        super::interactions::decode(REQUEST_VERIFICATION, answer)?;
    Ok(match decoded {
        InteractionAnswerRequestVerification::Alt1(token) => VerifyAnswer::Token(token.token),
        InteractionAnswerRequestVerification::Alt2(failed) => VerifyAnswer::Failure(failed.failure),
    })
}

/// 这个工作区的待校验索引 topic 的名字。
fn index_topic(key: &str) -> String {
    workspace_topic(key, WORKSPACE_VERIFICATIONS)
}

impl Actor {
    /// 开一个校验（§35.2）：会话在会话表里、有投影、没删除时 put 实体与索引行，否则应答口立即收到原因。
    pub(crate) fn verify(&mut self, verify: Verify) {
        let Verify {
            session,
            id,
            provider,
            challenge,
            expires_at,
            answer,
        } = verify;
        let Some(slot) = self.sessions.slots.get_mut(&session) else {
            let _ = answer.send(Err(NOT_OPEN.to_owned()));
            return;
        };
        let (Some(projection), Some(publisher)) =
            (slot.projection.as_mut(), self.topics.get_mut(&slot.topic))
        else {
            let _ = answer.send(Err(NOT_OPEN.to_owned()));
            return;
        };
        if projection.deleted() {
            let _ = answer.send(Err(NOT_OPEN.to_owned()));
            return;
        }
        let pending = Pending {
            provider,
            challenge,
            expires_at,
            answer,
        };
        let row = pending.row(&session);
        let mut batch = Batch::default();
        batch.put(INTERACTIONS, &id, pending.entity());
        projection.hold(&id, pending);
        if publisher.publish(batch) {
            self.outbox.mark_topic(&slot.topic);
        }
        let key = slot.key.clone();
        self.index(&key, &id, Some(row));
        self.live_changed(&session);
    }

    /// 撤回（任务放弃）：实体还在就移除，应答口随之丢掉。
    pub(crate) fn withdraw(&mut self, session: &str, id: &str) {
        let _ = self.settle(session, id);
    }

    /// 一条落在前端交互上的 `interaction.resolve`（§35.2）：实体还在 → 移除、交出应答、ACK accepted（排在帧之后）；不在 →
    /// `alreadyResolved`。按 id 裁决，不比 `v`：实体不变，`v` 只随快照重建变。
    pub(crate) fn answer_verification(&mut self, reply: &Value, front: FrontAnswer) {
        let workspace = self
            .sessions
            .slots
            .get(&front.sid)
            .map(|slot| slot.key.clone());
        if workspace.as_deref() != Some(front.key.as_str()) {
            let result = rejected(
                codes::PRECONDITION_WORKSPACE_MISMATCH,
                Some("target.workspaceKey".to_owned()),
                None,
            );
            return self.reply_command(reply, &result);
        }
        let result = match self.settle(&front.sid, &front.id) {
            Some((pending, effects)) => {
                pending.give(front.answer);
                CommandResult::Accepted(CommandResultAccepted {
                    effects,
                    noop: false,
                    result: None,
                })
            }
            None => *rejected(codes::PRECONDITION_ALREADY_RESOLVED, None, None),
        };
        self.reply_command(reply, &result);
    }

    fn reply_command(&mut self, reply: &Value, result: &CommandResult) {
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        self.outbox.reply(reply.clone(), Ok(value));
    }

    /// 移除一个待回答的校验：会话 topic 与索引同一次处理里移除、live 重算。返回它与会话 topic 的 cursor（命令的 effects）。
    fn settle(
        &mut self,
        session: &str,
        id: &str,
    ) -> Option<(Pending, Vec<CommandResultAcceptedEffectsItem>)> {
        let slot = self.sessions.slots.get_mut(session)?;
        let pending = slot.projection.as_mut()?.release(id)?;
        let mut effects = Vec::new();
        if let Some(publisher) = self.topics.get_mut(&slot.topic) {
            let mut batch = Batch::default();
            batch.remove(INTERACTIONS, id);
            if publisher.publish(batch) {
                self.outbox.mark_topic(&slot.topic);
            }
            let cursor = publisher.cursor();
            effects.push(CommandResultAcceptedEffectsItem {
                topic: slot.topic.clone(),
                epoch: cursor.epoch,
                seq: cursor.seq,
            });
        }
        let key = slot.key.clone();
        self.index(&key, id, None);
        self.live_changed(session);
        Some((pending, effects))
    }

    /// 索引里的一行变了（有 → put，没有 → remove）。索引还没有人订时不发：物化时从投影取当前的行。
    pub(crate) fn index(&mut self, key: &str, id: &str, row: Option<Fields>) {
        let topic = index_topic(key);
        let Some(publisher) = self.topics.get_mut(&topic) else {
            return;
        };
        let mut batch = Batch::default();
        match row {
            Some(fields) => batch.put(VERIFICATIONS, id, fields),
            None => batch.remove(VERIFICATIONS, id),
        }
        if publisher.publish(batch) {
            self.outbox.mark_topic(&topic);
        }
    }

    /// 订阅 `ws/<key>/verifications`（§35.2）：没登记就由这个工作区各会话的投影建出当前的行再登记。不组装工作区、不读库。
    pub(crate) fn ensure_index(&mut self, key: &str) {
        let topic = index_topic(key);
        if self.topics.contains_key(&topic) {
            return;
        }
        let rows: Vec<(String, Fields)> = self
            .sessions
            .slots
            .iter()
            .filter(|(_, slot)| slot.key == key)
            .filter_map(|(sid, slot)| Some((sid, slot.projection.as_ref()?)))
            .flat_map(|(sid, projection)| projection.verification_rows(sid))
            .collect();
        let mut publisher = Publisher::new(topic.clone(), self.epoch.clone());
        publisher.put(VERIFICATIONS, rows);
        self.topics.insert(topic, publisher);
    }

    /// 会话删除之后（`session.delete` 的回执）：投影已清掉它的校验，索引一并移除。
    pub(crate) fn unindex(&mut self, key: &str, ids: Vec<String>) {
        for id in ids {
            self.index(key, &id, None);
        }
    }
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
