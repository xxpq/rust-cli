//! actor 一步的耗时（protocol-v5.md §4.2 的第二部分）：手动运行的测量，不是门禁。N 个工作区 × M 个会话同时流式，每个内核事件交给
//! actor 当一步（同读循环里的一步：处理这条消息、写出站），记每一步的耗时。出站是真实的写者与 Content-Length 分帧，另一头有任务把
//! 字节读走（相当于 Host 读 stdout）。信用给足，不经 `credit` 消息。
//!
//! `cargo test --release -p rust-cli-protocol-v5 --lib actor_step -- --ignored --nocapture`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, MessageId, SessionId, TraceId, TurnId};
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Message};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    KernelEvent, SequencedEvent, SessionView, TurnOrigin, TurnOutcome,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};
use rust_cli_kernel_api::testing::MemoryStore;
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_core::framing::{Dialect, Framing};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, duplex};
use tokio::sync::mpsc;

use super::lane::LaneEvent;
use super::projection::Origin;
use crate::generated::core_v1::Subscribe;
use crate::server::{Actor, Event, Settings, Workspaces};
use crate::topics::{WORKSPACE_SESSIONS, session_topic, workspace_topic};

/// 规模：（工作区数，每个工作区的会话数）。
const SCALES: &[(usize, usize)] = &[(1, 1), (4, 4), (8, 8), (16, 8)];
/// 每个回复的段数与每段的文字。
const CHUNKS: usize = 200;
const CHUNK: &str = "xxxxxxxxxxxxxxxxxxxxxxxx";
/// 信用给足：这里只量 actor，不量 client 补信用的节奏。
const CREDIT: u64 = u64::MAX / 2;
/// 内存管道的缓冲（字节）。
const PIPE_BYTES: usize = 1 << 20;
const LOCAL: &str = "local";

/// 工作区的根目录（绝对路径；Windows 要盘符）。
fn root(index: usize) -> String {
    if cfg!(windows) {
        format!(r"C:\Users\dev\w{index}")
    } else {
        format!("/Users/dev/w{index}")
    }
}

fn workspace(key: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new(LOCAL),
        root: EnvPath::new(key),
    }
}

/// 入口的工作区端口：key 就是根目录；这里不组装内核（通道不会收到请求）。
struct Local;

#[async_trait::async_trait]
impl Workspaces for Local {
    async fn open(&self, _key: &str) -> Result<crate::server::WorkspaceKernel, String> {
        Ok(crate::test_link::kernel())
    }

    fn release(&self, _key: &str) -> bool {
        false
    }

    fn workspace(&self, key: &str) -> WorkspaceRef {
        workspace(key)
    }

    async fn mcp_pending(&self, _key: &str) -> Vec<crate::McpPending> {
        Vec::new()
    }

    async fn trust_mcp(
        &self,
        _key: &str,
        _decision: crate::McpDecision,
    ) -> Result<crate::McpTrusted, crate::McpTrustRefusal> {
        Err(crate::McpTrustRefusal::Unavailable(
            "no trust record".to_owned(),
        ))
    }
}

fn view(key: &str, sid: &str) -> SessionView {
    let meta = SessionMeta {
        id: SessionId::new(sid),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: workspace(key),
        title: String::new(),
        trace: TraceId::new(sid),
        created_at_ms: 1,
    };
    SessionView::new(
        meta,
        Vec::new(),
        rust_cli_kernel_api::testing::selection(),
        PermissionMode::Default,
    )
}

/// 一个会话在测量里的位置：通道的代、下一个内核 seq、回合与回复的 id。
struct Stream {
    sid: String,
    generation: u64,
    seq: u64,
    turn: TurnId,
    message: MessageId,
}

/// 每一步：处理一条通道消息、写出站。返回这一步的耗时。
async fn step(actor: &mut Actor, stream: &mut Stream, event: KernelEvent) -> Duration {
    stream.seq += 1;
    let event = LaneEvent::Kernel(Box::new(SequencedEvent {
        seq: stream.seq,
        event,
        revision: 0,
    }));
    let started = Instant::now();
    actor.on_lane(&stream.sid, stream.generation, event);
    actor.flush().await;
    started.elapsed()
}

fn micros(duration: Duration) -> u128 {
    duration.as_micros()
}

/// 最近秩的分位数：排好序的样本里第 ⌈p% × n⌉ 个。
fn rank(sorted: &[Duration], p: usize) -> Duration {
    let index = (sorted.len() * p).div_ceil(100).max(1) - 1;
    sorted[index]
}

async fn measure(workspaces: usize, sessions: usize) {
    let scope = TaskScope::root("load", |_| {});
    let options = Options {
        framing: Framing::ContentLength,
        dialect: Dialect::JsonRpc2,
        ..Options::default()
    };
    let (out, mut host) = duplex(PIPE_BYTES);
    let (peer, _writer) = connection::start(out, &scope.spawner(), options);
    let bytes = Arc::new(AtomicUsize::new(0));
    let read = bytes.clone();
    let reader = scope.spawner().spawn("host", async move {
        let mut buffer = vec![0; PIPE_BYTES];
        while let Ok(n) = host.read(&mut buffer).await {
            if n == 0 {
                break;
            }
            read.fetch_add(n, Ordering::Relaxed);
        }
    });
    let (events, mut inbox) = mpsc::unbounded_channel();
    let mut actor = Actor::new(peer, Settings::default(), &scope.spawner(), events);
    actor.process_ready(Arc::new(Local), Arc::new(MemoryStore::default()));

    let mut streams = Vec::new();
    for i in 0..workspaces {
        let key = root(i);
        let feed = format!("f{i}");
        let subscribe = Subscribe {
            topic: workspace_topic(&key, WORKSPACE_SESSIONS),
            sub: feed.clone(),
            cursor: None,
        };
        actor.subscribe_sessions(&json!(feed), subscribe, &key);
        let Some(Event::Seeded {
            key: seeded,
            result,
        }) = inbox.recv().await
        else {
            panic!("the seed task did not answer");
        };
        actor.seeded(&seeded, result);
        actor.outbox.credit(&feed, CREDIT);
        for j in 0..sessions {
            let sid = format!("ses_{i}_{j}");
            actor.lane(&key, &sid).expect("the process parts are ready");
            let generation = actor.sessions.slots[&sid]
                .lane
                .as_ref()
                .map_or(0, |lane| lane.1);
            let origin = Origin {
                live: true,
                draft: false,
                deleted: false,
                title_source: TitleSource::Default,
                mcp: Value::Null,
                subagents: Vec::new(),
                stamp: None,
                ..Default::default()
            };
            actor.on_lane(
                &sid,
                generation,
                LaneEvent::Viewed {
                    view: Box::new(view(&key, &sid)),
                    origin,
                },
            );
            let sub = format!("s{i}_{j}");
            actor
                .outbox
                .subscribe(json!(sub), &sub, &session_topic(&key, &sid), None);
            actor.outbox.credit(&sub, CREDIT);
            streams.push(Stream {
                turn: TurnId::new(format!("turn_{sid}")),
                message: MessageId::new(format!("msg_{sid}")),
                sid,
                generation,
                seq: 0,
            });
        }
    }
    actor.flush().await;

    let setup = bytes.load(Ordering::Relaxed);
    let started = Instant::now();
    let mut steps = Vec::new();
    for stream in &mut streams {
        let event = KernelEvent::TurnStarted {
            turn: stream.turn.clone(),
            command: Some(CommandId::new(format!("c_{}", stream.sid))),
            origin: TurnOrigin::Input,
            started_at: 1,
            works: Vec::new(),
        };
        steps.push(step(&mut actor, stream, event).await);
    }
    for _ in 0..CHUNKS {
        for stream in &mut streams {
            let event = KernelEvent::StreamDelta {
                turn: stream.turn.clone(),
                response: 0,
                message: stream.message.clone(),
                index: 0,
                tool_use: None,
                delta: BlockDelta::Text(CHUNK.to_owned()),
            };
            steps.push(step(&mut actor, stream, event).await);
        }
    }
    for stream in &mut streams {
        let message = Message::Assistant(AssistantMessage {
            id: stream.message.clone(),
            content: vec![ContentBlock::Text {
                text: CHUNK.repeat(CHUNKS),
            }],
            model: rust_cli_base::models::GLM_5_3.to_owned(),
            stop_reason: None,
            usage: None,
        });
        steps.push(step(&mut actor, stream, KernelEvent::MessageDone { message }).await);
        let finished = KernelEvent::TurnFinished {
            turn: stream.turn.clone(),
            outcome: TurnOutcome::Success,
            usage: None,
            time: rust_cli_kernel_api::session_api::TurnTime {
                ended_at: 0,
                active_ms: 0,
            },
            files: Vec::new(),
        };
        steps.push(step(&mut actor, stream, finished).await);
    }
    let busy: Duration = steps.iter().sum();
    // 丢掉 actor（连同它的 Peer）：写者写完队列就结束，读的一端读到结束，字节数是全部
    drop(actor);
    drop(reader.await);
    let wall = started.elapsed();
    let written = bytes.load(Ordering::Relaxed) - setup;
    steps.sort();
    let count = u128::try_from(steps.len()).unwrap_or(u128::MAX);
    println!(
        "actor_step {workspaces}x{sessions}: steps {}, p50 {} µs, p90 {} µs, p99 {} µs, max {} µs, mean {} µs; \
         busy {} ms of {} ms wall; {} steps/s busy; {} KiB out, {} KiB/s",
        steps.len(),
        micros(rank(&steps, 50)),
        micros(rank(&steps, 90)),
        micros(rank(&steps, 99)),
        micros(steps[steps.len() - 1]),
        micros(busy) / count,
        busy.as_millis(),
        wall.as_millis(),
        count * 1_000_000 / micros(busy).max(1),
        written / 1024,
        u128::try_from(written / 1024).unwrap_or(u128::MAX) * 1000 / wall.as_millis().max(1),
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "手动运行的测量"]
async fn actor_step() {
    for &(workspaces, sessions) in SCALES {
        measure(workspaces, sessions).await;
    }
}
