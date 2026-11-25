// 复制自 apps/zcode-cli-rust/crates/domain/src/node_ids.rs（feat/rs 79b6691c55）。
//! Node 会话库的 id 与 slug 格式（Node `contracts/src/interfaces/shared.ts`、
//! `core/src/runtime/helpers/project.ts`，docs/specs/rust-m11-node-storage.md §2.3）。
//!
//! 与复制来源的差异：只保留本 crate 用到的格式；新增 [`message_time`]，从消息 id 取创建时间。
//!
//! F7（审查 R1-15、R2-11）：Node 的 ID 格式只在这里。[`NodeIds`] 从 runtime 移来，内核（`Ports.ids`）与
//! 本 crate 写记录（part、fork 出的消息、turn、artifact）用同一个实例；它是全工程唯一生成随机 uuid 的地方
//! （[`RandomUuids`]）。以前 runtime 与本 crate 各有一份 36 进制、turn id 与随机源（`RecordSource`）。

use std::sync::Arc;

use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::ids::{
    AgentId, CommandId, Generation, InteractionId, MessageId, OperationId, RunId, SessionId, TurnId,
};
use sha2::Digest as _;

/// UUIDv7 里时间位之后的字节数（RFC 9562：版本与变体位由 uuid 的构造写入）。
const UUID_V7_RANDOM_BYTES: usize = 10;

/// 随机 uuid 的来源（定义在 node-records，云端的会话库也用它）：生产是 [`RandomUuids`]，测试注入顺序值与 Node 夹具逐字节比对。
pub use rust_cli_node_records::ids::UuidSource;

/// uuid v4。
pub struct RandomUuids;

impl UuidSource for RandomUuids {
    #[expect(
        clippy::disallowed_methods,
        reason = "NodeIds 的随机源是全工程唯一生成 uuid 的地方（README §4 的 ID 类入口）"
    )]
    fn uuid(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Node 格式的 ID：`sess_<uuid>`、`msg_<36 进制毫秒>_<uuid>`、`turn_<uuid>`、`agent_<uuid>` 与
/// `sess_subagent_agent_<uuid>`。Node 没有的 ID（run、operation、interaction）用 `<前缀>_<uuid>`；命令与执行环境
/// 实例是不带前缀的 uuid。带着注入的时钟：消息 ID 里的毫秒与记录的时间同源。
pub struct NodeIds {
    clock: Arc<dyn Clock>,
    uuids: Arc<dyn UuidSource>,
}

impl NodeIds {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_uuids(clock, Arc::new(RandomUuids))
    }

    pub fn with_uuids(clock: Arc<dyn Clock>, uuids: Arc<dyn UuidSource>) -> Self {
        Self { clock, uuids }
    }

    /// 注入的时钟的当前时间（记录的 `time_*`）。
    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// 一个新的 uuid（part、fork 出的消息、artifact 的 ID 里的随机部分；外部编辑器的临时文件名）。
    pub fn uuid(&self) -> String {
        self.uuids.uuid()
    }
}

/// 云端的会话库写记录时取同一个来源（pg-store 的 `bind`）。
impl UuidSource for NodeIds {
    fn uuid(&self) -> String {
        self.uuids.uuid()
    }
}

impl Ids for NodeIds {
    fn session(&self) -> SessionId {
        SessionId::new(format!("sess_{}", self.uuid()))
    }

    fn subagent(&self) -> (AgentId, SessionId) {
        let id = self.uuid();
        (
            AgentId::new(format!("agent_{id}")),
            SessionId::new(format!("sess_subagent_agent_{id}")),
        )
    }

    fn message(&self) -> MessageId {
        MessageId::new(message_id(self.now_ms(), &self.uuid()))
    }

    fn turn(&self) -> TurnId {
        TurnId::new(turn_id(&self.uuid()))
    }

    fn run(&self) -> RunId {
        RunId::new(format!("run_{}", self.uuid()))
    }

    fn operation(&self) -> OperationId {
        OperationId::new(format!("op_{}", self.uuid()))
    }

    fn interaction(&self) -> InteractionId {
        InteractionId::new(format!("interaction_{}", self.uuid()))
    }

    /// UUIDv7（session-lifecycle.md §10.2，K-A3）：时间位是注入的时钟，随机位取自 [`UuidSource`] 的一个新 uuid（全工程
    /// 唯一的随机源），所以测试注入的顺序值同样确定。命令账本跨进程之后，按内容拼的 id 会在下一个进程里重复。
    fn command(&self) -> CommandId {
        let digest = sha2::Sha256::digest(self.uuid().as_bytes());
        let mut random = [0u8; UUID_V7_RANDOM_BYTES];
        random.copy_from_slice(&digest[..UUID_V7_RANDOM_BYTES]);
        CommandId::new(
            uuid::Builder::from_unix_timestamp_millis(self.now_ms(), &random)
                .into_uuid()
                .to_string(),
        )
    }

    fn generation(&self) -> Generation {
        Generation::new(self.uuid())
    }
}

pub use rust_cli_node_records::ids::{message_id, project_id, turn_id};

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
