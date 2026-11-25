//! 连接上的共用部分：往 Host 写帧（wire v3、每个订阅自己的逻辑帧序号）、时间与随机标识。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rust_cli_kernel_api::clock::Clock;
use rust_cli_protocol_core::connection::Peer;
use serde_json::{Value, json};

use crate::wire::{self, Envelope, Purpose};

/// 帧的通知名。
pub const FRAME: &str = "v4/conversation/frame";

/// 写帧的一方与时间。会话投影、会话列表与工作区配置的发布者共用。
pub struct Context {
    pub peer: Peer,
    pub clock: Arc<dyn Clock>,
    ordinals: Mutex<HashMap<String, u64>>,
    counter: Mutex<u64>,
    /// 路由表：会话 → 所在的工作区（Host 的 workspacePath；app-protocol.md §15.1，工作线 F145）。建会话与订阅时登记，
    /// 粘到进程退出；会话请求按它找内核，反向请求按它带工作区。
    routes: Mutex<HashMap<String, String>>,
    /// Host 声明了它按工作区路由（`hostCapabilities.sessionWorkspaceRouting`，只升不降）。
    host_routes: AtomicBool,
    /// 本进程的随机前缀（代际标识与订阅 ID 用）。
    prefix: String,
}

/// 登记冲突：这个会话已经归另一个工作区（值是那个工作区）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteConflict(pub String);

impl Context {
    pub fn new(peer: Peer, clock: Arc<dyn Clock>) -> Self {
        let prefix = format!(
            "{:x}-{:x}",
            clock.now_ms(),
            rust_cli_base::random::u64() & u64::from(u32::MAX)
        );
        Self {
            peer,
            clock,
            ordinals: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
            routes: Mutex::new(HashMap::new()),
            host_routes: AtomicBool::new(false),
            prefix,
        }
    }

    /// 登记会话所在的工作区：没有登记时记下；已经是这个工作区时什么也不做；已经归别的工作区时拒绝（不改）。
    pub fn route(&self, session: &str, workspace: &str) -> Result<(), RouteConflict> {
        let mut routes = self.routes.lock_unpoisoned();
        match routes.get(session) {
            Some(known) if known != workspace => Err(RouteConflict(known.clone())),
            Some(_) => Ok(()),
            None => {
                routes.insert(session.to_owned(), workspace.to_owned());
                Ok(())
            }
        }
    }

    /// 会话所在的工作区；没有登记时为 None。
    pub fn workspace_of(&self, session: &str) -> Option<String> {
        self.routes.lock_unpoisoned().get(session).cloned()
    }

    /// Host 声明了它按工作区路由：之后发的反向请求都带 `workspace`（app-protocol.md §15.4）。
    pub fn set_host_routes(&self) {
        self.host_routes.store(true, Ordering::Relaxed);
    }

    pub fn host_routes(&self) -> bool {
        self.host_routes.load(Ordering::Relaxed)
    }

    pub fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    /// 本进程里唯一的标识：`<kind>-<进程前缀>-<n>`（代际标识、订阅 ID）。
    pub fn fresh(&self, kind: &str) -> String {
        let mut counter = self.counter.lock_unpoisoned();
        *counter += 1;
        format!("{kind}-{}-{}", self.prefix, *counter)
    }

    fn ordinal(&self, subscription: &str) -> u64 {
        let mut ordinals = self.ordinals.lock_unpoisoned();
        let ordinal = ordinals.entry(subscription.to_owned()).or_default();
        *ordinal += 1;
        *ordinal
    }

    /// 订阅结束：逻辑帧序号不再需要。
    pub fn forget(&self, subscription: &str) {
        self.ordinals.lock_unpoisoned().remove(subscription);
    }

    /// 写一个逻辑帧（`topicFrame`）：快照或一段增量。
    pub async fn frame(
        &self,
        topic: &str,
        subscription: &str,
        (from, to): (u64, u64),
        payload: Value,
        purpose: Purpose,
    ) {
        let frame = json!({
            "topic": topic,
            "subscriptionId": subscription,
            "fromSeq": from,
            "toSeq": to,
            "sentAt": self.now(),
            "payload": payload,
        });
        let envelope = Envelope {
            topic,
            subscription,
            ordinal: self.ordinal(subscription),
            purpose,
        };
        let Some(physical) = wire::encode(&frame, &envelope) else {
            tracing::warn!(
                event = "protocol.frame.too_large",
                topic,
                "a logical frame exceeded the assembly limit and was dropped"
            );
            return;
        };
        for wire in physical {
            self.peer.notify(FRAME, &wire).await;
        }
    }
}
