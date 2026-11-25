//! 按连接全序的出站队列（01 §4.3 背压、§7.3 效果先于 ACK、06 G2；与 protocol-runtime `server/outbox.ts` 同一算法）：
//!
//! - 帧标记：某个订阅“有新内容待发”。轮到它时按订阅已发到的位置取续传帧或快照；订阅没有信用就停在队头，排在它后面的
//!   一切（含命令的回复）一起等。
//! - 回复：命令的 ACK 与 `subscribed`，排在当时已入队的帧之后。
//! - 通知：源读不出时给等着的订阅的 `subscription.failed`（会话列表，protocol-v5.md §14.4），同样按队列的顺序。
//!
//! 纯状态机：[`Outbox::pump`] 返回要按顺序写出的消息，写由 actor 做。帧编码成文字一次：量字节与写出都用这一份（protocol-v5.md §68.1）。

use std::collections::{BTreeMap, VecDeque};

use rust_cli_protocol_core::rpc::RpcError;
use serde_json::Value;

use crate::errors::{not_applied, protocol_error};
use crate::generated::contract::{FRAME, SUBSCRIPTION_FAILED, codes};
use crate::generated::core_v1::{
    Cursor, FrameBody, ProtocolError, Subscribed, SubscribedPending, SubscriptionFailed,
};
use crate::publisher::Publisher;

/// 一个订阅。
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub topic: String,
    /// 已经发给它的位置（没有帧时是订阅请求里的 cursor）。
    pub at: Option<Cursor>,
    pub credit: u64,
    marked: bool,
}

#[derive(Debug)]
enum Entry {
    Frame(String),
    Answer(Answer),
    Notice(Outgoing),
}

#[derive(Debug)]
enum Answer {
    Reply {
        id: Value,
        result: Result<Value, RpcError>,
    },
    /// `subscribed`：cursor 取轮到它时订阅已发到的位置（订阅已经没了时取入队时的位置）。
    Subscribed {
        id: Value,
        sub: String,
        at: Option<Cursor>,
    },
    /// `subscribed{pending: source_unavailable}`（01 §5.2）：源还没有，登记之后订阅拿快照。
    Pending { id: Value, sub: String },
}

/// 要写出的一条消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    Notify {
        method: &'static str,
        params: Value,
    },
    /// 一帧：params 已经编码成文字（量字节时编的那一份，§68.1）。
    Frame {
        params: String,
    },
    Respond {
        id: Value,
        result: Result<Value, RpcError>,
    },
}

/// 一条通知的 JSON-RPC 外壳（`{"jsonrpc":"2.0","method":"<方法>","params":}`）的字节数，加上 params 的文字就是整条消息。
fn notification_bytes(method: &str, params: &str) -> usize {
    const ENVELOPE: &str = r#"{"jsonrpc":"2.0","method":"","params":}"#;
    ENVELOPE.len() + method.len() + params.len()
}

pub struct Outbox {
    subs: BTreeMap<String, Subscription>,
    queue: VecDeque<Entry>,
    initial_credit: u64,
    max_message_bytes: usize,
    /// shutdown 之后（03 §3.4 ②）：不再挂新的帧标记，已挂的照发。
    closed: bool,
    /// 这条连接协商出 `core.patch`（01 §19.4）：发 entity.patch；否则同一项发当前整行的 put。握手时定。
    patch: bool,
}

impl Outbox {
    pub fn new(initial_credit: u64, max_message_bytes: usize) -> Self {
        Self {
            subs: BTreeMap::new(),
            queue: VecDeque::new(),
            initial_credit,
            max_message_bytes,
            closed: false,
            patch: false,
        }
    }

    /// 握手定下：这条连接收不收 entity.patch。
    pub fn allow_patch(&mut self, patch: bool) {
        self.patch = patch;
    }

    /// 这个 topic 每个在订订阅都已收到的位置（同一 epoch 里最小的那个；没有订阅、或订阅还没有这个 epoch 的位置时取 `current`）：
    /// 换骨架只换不晚于它的行（protocol-v5.md §68.0 第 5 条）。不同 epoch 的订阅之后一定拿快照，不用为它留整行。
    pub fn sent_floor(&self, topic: &str, current: &Cursor) -> u64 {
        self.subs
            .values()
            .filter(|s| s.topic == topic)
            .filter_map(|s| s.at.as_ref().filter(|at| at.epoch == current.epoch))
            .map(|at| at.seq)
            .min()
            .unwrap_or(current.seq)
    }

    #[cfg(test)]
    pub fn subscription(&self, sub: &str) -> Option<&Subscription> {
        self.subs.get(sub)
    }

    /// 这个订阅（含等物化的）指向的 topic：退订、改订之后对它复查会话投影的释放（protocol-v5.md §66.0 第 4 条）。
    pub fn topic_of(&self, sub: &str) -> Option<&str> {
        self.subs.get(sub).map(|s| s.topic.as_str())
    }

    /// 订阅（同一个 sub 再订：替换，按新的 cursor 重新同步）：挂一个帧标记，`subscribed` 排在它后面。同一个 sub 再订时信用与
    /// 队列里的标记都继承（01 §4.3、§5.2：重订之前发出的帧还在途，client 收到后照样补信用，重置再叠加就超出窗口；标记留在
    /// 原位，新首帧仍先于之后的 ACK）。
    pub fn subscribe(&mut self, id: Value, sub: &str, topic: &str, cursor: Option<Cursor>) {
        self.hold(sub, topic, cursor.clone());
        self.mark(sub);
        self.subscribed(id, sub, cursor);
    }

    /// 记下一个订阅，先不挂标记、不回复：topic 正在物化（protocol-v5.md §14.4）。登记时 [`Self::mark_topic`] 给它挂标记，
    /// 之后 [`Self::subscribed`] 排它的回复；期间退订、重订照常改这里的记录。信用与重订的规则同 [`Self::subscribe`]。
    pub fn hold(&mut self, sub: &str, topic: &str, cursor: Option<Cursor>) {
        self.register(sub, topic, cursor);
    }

    /// 排一个 `subscribed`（在当前全部条目之后）：cursor 取轮到它时订阅已发到的位置；订阅已经没了时取 `at`（请求里的）。
    pub fn subscribed(&mut self, id: Value, sub: &str, at: Option<Cursor>) {
        self.queue.push_back(Entry::Answer(Answer::Subscribed {
            id,
            sub: sub.to_owned(),
            at,
        }));
    }

    /// 物化失败（protocol-v5.md §14.4）：这个订阅请求回错误；订阅还指向这个 topic 时删掉它（期间重订到别的 topic 的不动）。
    pub fn refuse(&mut self, id: Value, sub: &str, topic: &str, error: RpcError) {
        if self.subs.get(sub).is_some_and(|s| s.topic == topic) {
            self.subs.remove(sub);
        }
        self.reply(id, Err(error));
    }

    /// 订阅一个还没有登记的 topic（源暂时不可用，01 §5.2）：先回 `pending`，不挂帧标记；topic 登记时 [`Self::mark_topic`]
    /// 给它挂上，首帧是快照（它的 cursor 不可能是新 topic 的）。信用与重订的规则同 [`Self::subscribe`]。
    pub fn subscribe_pending(&mut self, id: Value, sub: &str, topic: &str, cursor: Option<Cursor>) {
        self.register(sub, topic, cursor);
        self.pending(id, sub);
    }

    /// 排一个 `pending`（在当前全部条目之后），不改订阅的记录：先 [`Self::hold`] 着的订阅，源原来以为在、复核之后不在了
    /// （保留的会话投影过时，protocol-v5.md §70）。之后同 [`Self::subscribe_pending`]：登记时拿快照。
    pub fn pending(&mut self, id: Value, sub: &str) {
        self.queue.push_back(Entry::Answer(Answer::Pending {
            id,
            sub: sub.to_owned(),
        }));
    }

    fn register(&mut self, sub: &str, topic: &str, cursor: Option<Cursor>) {
        let (credit, marked) = self
            .subs
            .get(sub)
            .map_or((self.initial_credit, false), |previous| {
                (previous.credit, previous.marked)
            });
        self.subs.insert(
            sub.to_owned(),
            Subscription {
                topic: topic.to_owned(),
                at: cursor,
                credit,
                marked,
            },
        );
    }

    /// 等着这个 topic 的订阅都结束：源读不出（protocol-v5.md §14.4）。每个订阅排一条 `subscription.failed`；关闭之后不排
    /// （shutdown 的回复之后不再有消息，03 §3.4）。
    pub fn fail_topic(&mut self, topic: &str, error: &ProtocolError) {
        if self.closed {
            return;
        }
        let subs: Vec<String> = self
            .subs
            .iter()
            .filter(|(_, s)| s.topic == topic)
            .map(|(sub, _)| sub.clone())
            .collect();
        for sub in subs {
            self.subs.remove(&sub);
            let failed = SubscriptionFailed {
                sub,
                error: error.clone(),
            };
            self.queue.push_back(Entry::Notice(Outgoing::Notify {
                method: SUBSCRIPTION_FAILED,
                params: serde_json::to_value(failed).unwrap_or(Value::Null),
            }));
        }
    }

    /// 有没有订阅（含还在等物化的）指向这个 topic：会话投影的释放看它（protocol-v5.md §66.2）。
    pub fn watched(&self, topic: &str) -> bool {
        self.subs.values().any(|s| s.topic == topic)
    }

    /// 退订。没有这个订阅时返回假。之后要 pump：队头可能正被它挡着（审查 F08）。
    pub fn unsubscribe(&mut self, sub: &str) -> bool {
        self.subs.remove(sub).is_some()
    }

    pub fn credit(&mut self, sub: &str, n: u64) {
        if let Some(s) = self.subs.get_mut(sub) {
            s.credit = s.credit.saturating_add(n);
        }
    }

    /// shutdown（03 §3.4）：之后的发布不再挂标记，所以排在 shutdown 回复之后的不会有帧；已挂的标记与回复照常按顺序出去。
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// 某个 topic 有新内容：给订阅它的每个 sub 挂一个标记（已经有的不重复）。关闭之后不挂。
    pub fn mark_topic(&mut self, topic: &str) {
        if self.closed {
            return;
        }
        let subs: Vec<String> = self
            .subs
            .iter()
            .filter(|(_, s)| s.topic == topic)
            .map(|(sub, _)| sub.clone())
            .collect();
        for sub in subs {
            self.mark(&sub);
        }
    }

    fn mark(&mut self, sub: &str) {
        if let Some(s) = self.subs.get_mut(sub)
            && !s.marked
        {
            s.marked = true;
            self.queue.push_back(Entry::Frame(sub.to_owned()));
        }
    }

    /// 排在当前全部条目之后的回复（命令的 ACK：效果帧的标记已经在前面）。
    pub fn reply(&mut self, id: Value, result: Result<Value, RpcError>) {
        self.queue
            .push_back(Entry::Answer(Answer::Reply { id, result }));
    }

    /// 队头被没有信用的订阅挡住。
    #[cfg(test)]
    pub fn blocked(&self) -> bool {
        matches!(self.queue.front(), Some(Entry::Frame(sub)) if self.subs.get(sub).is_some_and(|s| s.credit == 0))
    }

    /// 能写的都取出来（按顺序）；队头被挡住时停下。
    pub fn pump(&mut self, topics: &BTreeMap<String, Publisher>) -> Vec<Outgoing> {
        let mut out = Vec::new();
        while let Some(entry) = self.queue.pop_front() {
            match entry {
                Entry::Frame(sub) => {
                    let Some(s) = self.subs.get(&sub) else {
                        continue;
                    };
                    if s.credit == 0 {
                        // 没有信用：放回队头，排在后面的一切一起等（G2）
                        self.queue.push_front(Entry::Frame(sub));
                        break;
                    }
                    if let Some(message) = self.frame(&sub, topics) {
                        out.push(message);
                    }
                }
                Entry::Answer(answer) => out.push(self.respond(answer)),
                Entry::Notice(notice) => out.push(notice),
            }
        }
        out
    }

    fn respond(&self, answer: Answer) -> Outgoing {
        match answer {
            Answer::Reply { id, result } => Outgoing::Respond { id, result },
            Answer::Subscribed { id, sub, at } => {
                let cursor = self.subs.get(&sub).map_or(at, |s| s.at.clone());
                let subscribed = Subscribed {
                    sub,
                    cursor,
                    pending: None,
                };
                Outgoing::Respond {
                    id,
                    result: Ok(serde_json::to_value(subscribed).unwrap_or(Value::Null)),
                }
            }
            Answer::Pending { id, sub } => {
                let pending = Subscribed {
                    sub,
                    cursor: None,
                    pending: Some(SubscribedPending::SourceUnavailable),
                };
                Outgoing::Respond {
                    id,
                    result: Ok(serde_json::to_value(pending).unwrap_or(Value::Null)),
                }
            }
        }
    }

    /// 轮到一个订阅的帧：已是最新时不发；超过上限时改发快照，快照仍超限时结束这个订阅（03 §3.2，审查 F10）。
    fn frame(&mut self, sub: &str, topics: &BTreeMap<String, Publisher>) -> Option<Outgoing> {
        let s = self.subs.get_mut(sub)?;
        s.marked = false;
        let publisher = topics.get(&s.topic)?;
        if s.at.as_ref() == Some(&publisher.cursor()) {
            return None;
        }
        let mut frame = publisher.frame_for(s.at.as_ref(), sub, self.patch);
        let mut text = serde_json::to_string(&frame).unwrap_or_default();
        if notification_bytes(FRAME, &text) > self.max_message_bytes
            && matches!(frame.body, FrameBody::Deltas(_))
        {
            frame = publisher.frame_for(None, sub, self.patch);
            text = serde_json::to_string(&frame).unwrap_or_default();
        }
        if notification_bytes(FRAME, &text) > self.max_message_bytes {
            let bytes = notification_bytes(FRAME, &text);
            self.subs.remove(sub);
            let error = not_applied(protocol_error(
                codes::LINK_MESSAGE_TOO_LARGE,
                format!(
                    "a frame of {bytes} bytes exceeds the link limit of {} bytes",
                    self.max_message_bytes
                ),
            ));
            let failed = SubscriptionFailed {
                sub: sub.to_owned(),
                error,
            };
            return Some(Outgoing::Notify {
                method: SUBSCRIPTION_FAILED,
                params: serde_json::to_value(failed).unwrap_or(Value::Null),
            });
        }
        s.at = Some(Cursor {
            epoch: frame.epoch.clone(),
            seq: frame.to,
        });
        s.credit -= 1;
        // 修 bug（评审 I5）：以前这里把量字节时编好的文字解析回 Value、写出时再编一次。帧原样交出这一份文字
        Some(Outgoing::Frame { params: text })
    }
}

#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;
