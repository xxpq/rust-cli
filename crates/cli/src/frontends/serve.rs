//! `zcode-rs serve`（docs/specs/rust-cli/multi-tenant.md §4、headless.md §8）：常驻进程，一条 WS 连接跑一轮。
//!
//! 进程级只有一份：事实、时钟、出口、PG 连接池、轮次表。每条连接：认运行壳的服务凭据 → 第一条消息（`session_open`
//! 或 `session_attach`）→ 跑一轮（[`turn`]）或续上 → 把这一轮的输出发给连接。连接的读另起一个任务（WS 读帧不能被
//! `select!` 中途取消），发送循环只在“读到的消息”与“这一轮有了新行”两样上等。连接断开后这一轮继续跑，宽限到了还没人
//! 续上就打断（[`turns`]）。收到 SIGTERM 时不再接受连接，等手上的轮跑完、宽限的计时到期再退出。
mod protocol;
mod turn;
mod turns;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rust_cli_base::names::DOT_DIR;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_base::{Spawner, Task, TaskScope};
use rust_cli_net::websocket::{CLOSE_NORMAL, WsFrame, WsReader, WsWriter};
use rust_cli_net::ws_server::{Incoming, Upgrade, WsListener};
use serde_json::json;
use tokio::sync::mpsc;

use self::protocol::{First, code};
use self::turn::Shared;
use self::turns::{Expiry, Since, TurnKey, TurnLog};
use crate::assemble::{session_pg, setup};
use crate::commands::flags;
use crate::process::output::print_line;
use crate::process::{ControllerRun, Start};

/// 连接池的缺省上限（`--pg-pool`）。
const DEFAULT_PG_POOL: u32 = 8;
/// 断开后宽限的缺省秒数（`--grace`，同运行壳的 `-detach-grace`）。
const DEFAULT_GRACE_SECS: u64 = 120;
/// 一条 WS 消息最多这么多字节（握手里的追加 system prompt 运行壳限 100 KiB）。
const MAX_MESSAGE_BYTES: u64 = 4 * 1024 * 1024;
/// 读的一侧交给发送循环的消息最多排这么多条。
const INBOX: usize = 16;

/// 子命令的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// `--listen <host:port>`。
    pub(crate) listen: String,
    /// `--token-env <NAME>`：放运行壳服务凭据的环境变量。
    pub(crate) token_env: String,
    /// `--pg-url-env <NAME>`：放 PG 连接串的环境变量；不给时每个用户一个本机库。
    pub(crate) pg_url_env: Option<String>,
    /// `--pg-pool <N>`。
    pub(crate) pg_pool: u32,
    /// `--grace <秒>`。
    pub(crate) grace: Duration,
    /// `--data-dir <目录>`：每个用户的目录在它的 `users/` 下；不给时是 home 下的 `.zcode/rust-cli/serve`。
    pub(crate) data_dir: Option<String>,
}

/// `serve` 子命令；不是它时为 None。
pub(crate) fn parse(args: &[String]) -> Option<Result<Parsed, String>> {
    let (first, rest) = args.split_first()?;
    if first != "serve" {
        return None;
    }
    Some(parse_options(rest))
}

fn parse_options(rest: &[String]) -> Result<Parsed, String> {
    let mut listen = None;
    let mut token_env = None;
    let mut pg_url_env = None;
    let mut pg_pool = DEFAULT_PG_POOL;
    let mut grace = Duration::from_secs(DEFAULT_GRACE_SECS);
    let mut data_dir = None;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or_else(|| format!("error: option '{name}' argument missing"))
        };
        match name {
            "--listen" => listen = Some(value()?),
            "--token-env" => token_env = Some(value()?),
            "--pg-url-env" => pg_url_env = Some(value()?),
            "--pg-pool" => {
                pg_pool = value()?
                    .parse()
                    .ok()
                    .filter(|pool| *pool > 0)
                    .ok_or("error: option '--pg-pool' needs a positive number")?;
            }
            "--grace" => {
                let seconds: u64 = value()?
                    .parse()
                    .map_err(|_| "error: option '--grace' needs a number of seconds")?;
                grace = Duration::from_secs(seconds);
            }
            "--data-dir" => data_dir = Some(value()?),
            other => return Err(format!("error: unknown option '{other}'")),
        }
    }
    Ok(Parsed {
        listen: listen.ok_or("error: serve needs --listen <host:port>")?,
        token_env: token_env.ok_or("error: serve needs --token-env <NAME>")?,
        pg_url_env,
        pg_pool,
        grace,
        data_dir,
    })
}

pub(crate) async fn run(parsed: Parsed) -> Result<u8> {
    // 日志只写文件：stdout 只写一行就绪（给部署与测试找端口）。
    let run = ControllerRun::begin(Start {
        mode: "serve",
        surface: None,
    })?;
    let result = serve(parsed, &run).await;
    run.finish(result).await
}

async fn serve(parsed: Parsed, run: &ControllerRun) -> Result<u8> {
    let facts = &run.facts;
    let token = facts
        .process
        .env
        .text(&parsed.token_env)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            flags::Fatal(format!(
                "Error: serve --token-env names {}, which is not set",
                parsed.token_env
            ))
        })?;
    let pg = match &parsed.pg_url_env {
        Some(name) => {
            let url = facts.process.env.text(name).ok_or_else(|| {
                flags::Fatal(format!(
                    "Error: serve --pg-url-env names {name}, which is not set"
                ))
            })?;
            let database =
                session_pg::connect(&RedactedString::new(url.to_owned()), parsed.pg_pool).await?;
            Some(Arc::new(database))
        }
        None => None,
    };
    // 出口在 serve 启动时建一次、各轮共用：CA 证书读不出时同 `-p`，启动就退出（config.md §2.4.1）。
    let egress = setup::egress(&facts.process, setup::OnCaUnreadable::Exit).await?;
    let data_root = match &parsed.data_dir {
        Some(dir) => facts.process.cwd.join(dir),
        None => facts
            .process
            .home
            .join(DOT_DIR)
            .join("rust-cli")
            .join("serve"),
    };
    let listener = WsListener::bind(&parsed.listen)
        .await
        .with_context(|| format!("listening on {}", parsed.listen))?;
    let address = listener
        .local_addr()
        .context("reading the listen address")?;
    let shared = Arc::new(Shared {
        facts: facts.clone(),
        clock: run.clock.clone(),
        egress,
        pg,
        data_root,
        grace: parsed.grace,
        token: RedactedString::new(token.to_owned()),
        turns: turns::Turns::default(),
    });
    let scope = TaskScope::root("serve", |panic| {
        tracing::error!(scope = %panic.scope, task = panic.task, message = %panic.message, "task panicked");
    });
    let tasks = Tasks::default();
    tracing::info!(event = "serve.ready", listen = %address, pool = parsed.pg_pool, grace_secs = parsed.grace.as_secs());
    print_line(&json!({"type": "serve_ready", "listen": address.to_string()}).to_string());
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(incoming) => {
                    let task = scope.spawner().spawn(
                        "serve-connection",
                        connection(shared.clone(), scope.spawner(), tasks.clone(), incoming),
                    );
                    tasks.keep(task);
                }
                Err(error) => tracing::warn!(event = "serve.accept_failed", %error),
            },
            () = run.stop.token.cancelled() => break,
        }
    }
    // 排空：不再接受连接；手上的轮照常跑完、宽限的计时到期（SIGTERM 不打断进行中的轮，硬停交给部署的宽限期）。
    tracing::info!(event = "serve.drain");
    drop(listener);
    tasks.drain().await;
    Ok(0)
}

/// serve 起的任务（连接、轮、宽限的计时）：排空时等它们都结束。
#[derive(Clone, Default)]
struct Tasks(Arc<std::sync::Mutex<Vec<Task<()>>>>);

impl Tasks {
    fn keep(&self, task: Task<()>) {
        let mut tasks = self.0.lock_unpoisoned();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    async fn drain(&self) {
        loop {
            let batch = std::mem::take(&mut *self.0.lock_unpoisoned());
            if batch.is_empty() {
                return;
            }
            for task in batch {
                let _ = task.await;
            }
        }
    }
}

/// 一条连接：认凭据、读第一条消息、跑一轮或续上。
async fn connection(shared: Arc<Shared>, spawner: Spawner, tasks: Tasks, incoming: Incoming) {
    let peer = incoming.peer();
    let upgraded = incoming
        .upgrade(MAX_MESSAGE_BYTES, |upgrade| {
            authorize(upgrade, &shared.token)
        })
        .await;
    let (socket, _) = match upgraded {
        Ok(upgraded) => upgraded,
        Err(error) => {
            tracing::info!(event = "serve.refused", %peer, %error);
            return;
        }
    };
    let (mut reader, mut writer) = socket.split();
    let Some(first) = next_text(&mut reader, &mut writer).await else {
        return;
    };
    match protocol::first(&first) {
        Err(message) => refuse(&mut writer, code::BAD_REQUEST, &message).await,
        Ok(First::SessionOpen(open)) => {
            let plan = match turn::plan(&shared, &open) {
                Ok(plan) => plan,
                Err(message) => return refuse(&mut writer, code::BAD_REQUEST, &message).await,
            };
            // 握手之后是一行 stream-json 的 user 消息（规则同 `--input-format stream-json`）。
            let prompt = loop {
                let Some(line) = next_text(&mut reader, &mut writer).await else {
                    return;
                };
                match rust_cli_headless::parse_user_line(&line) {
                    Ok(Some(prompt)) => break prompt,
                    Ok(None) => {}
                    Err(message) => {
                        return refuse(&mut writer, code::BAD_REQUEST, &message).await;
                    }
                }
            };
            let key = TurnKey {
                user: open.user.clone(),
                turn_id: open.turn_id.clone(),
            };
            let Some(log) = shared.turns.begin(key) else {
                return refuse(
                    &mut writer,
                    code::TURN_EXISTS,
                    "this turn is already running",
                )
                .await;
            };
            tracing::info!(event = "serve.turn.start", user = %open.user, turn = %open.turn_id);
            let turn = spawner.spawn(
                "serve-turn",
                turn::run(shared.clone(), plan, prompt, log.clone()),
            );
            tasks.keep(turn);
            pump(&shared, (&spawner, &tasks), log, 0, (reader, writer)).await;
        }
        Ok(First::SessionAttach(attach)) => {
            let key = TurnKey {
                user: attach.user,
                turn_id: attach.turn_id,
            };
            match shared.turns.find(&key) {
                Some(log) => {
                    tracing::info!(event = "serve.turn.attach", user = %key.user, turn = %key.turn_id, after = attach.after_seq);
                    pump(
                        &shared,
                        (&spawner, &tasks),
                        log,
                        attach.after_seq,
                        (reader, writer),
                    )
                    .await;
                }
                None => {
                    refuse(
                        &mut writer,
                        code::TURN_NOT_FOUND,
                        "no such turn in this process",
                    )
                    .await;
                }
            }
        }
    }
}

/// 认运行壳的服务凭据（`Authorization: Bearer <凭据>`），比较时间不随内容变化。
fn authorize(upgrade: &Upgrade, token: &RedactedString) -> Result<(), u16> {
    let expected = format!("Bearer {}", token.expose());
    let given = upgrade.header("authorization").unwrap_or_default();
    let same = given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0;
    if same {
        Ok(())
    } else {
        Err(rust_cli_base::proto::http::UNAUTHORIZED)
    }
}

/// 读下一条文本消息（握手阶段还没有发送循环，直接读；ping 的 pong 由 net 用的库在读的时候回）。连接关了或读错时为 None。
async fn next_text(reader: &mut WsReader, writer: &mut WsWriter) -> Option<String> {
    loop {
        match reader.next().await {
            Ok(WsFrame::Text(text)) => return Some(text),
            Ok(WsFrame::Close { code, .. }) => {
                writer.close(code).await;
                return None;
            }
            Ok(WsFrame::Binary(_)) => {}
            Err(_) => return None,
        }
    }
}

/// 回一个错误帧，关连接。
async fn refuse(writer: &mut WsWriter, code: &str, message: &str) {
    let _ = writer
        .send_text(&protocol::error_frame(code, message))
        .await;
    writer.close(CLOSE_NORMAL).await;
}

/// 读的一侧交给发送循环的东西（ping 的 pong 由 net 用的库在读的时候回，不经这里）。
enum Inbound {
    Text(String),
    /// 对方关了或读错。
    Closed(Option<u16>),
}

/// 把这一轮从 `after` 之后的输出发给连接，直到这一轮跑完、发完（关连接，交完了就从表里拿掉），或连接断开（开始宽限）。
async fn pump(
    shared: &Arc<Shared>,
    (spawner, tasks): (&Spawner, &Tasks),
    log: Arc<TurnLog>,
    after: u64,
    (reader, mut writer): (WsReader, WsWriter),
) {
    log.attach();
    let (inbox, mut inbound) = mpsc::channel(INBOX);
    let reading = spawner.spawn("serve-read", read(reader, inbox));
    let mut progress = log.watch();
    let mut sent = after;
    let delivered = 'pump: loop {
        match log.since(sent) {
            Since::Truncated => {
                let _ = writer
                    .send_text(&protocol::error_frame(
                        code::EVENTS_TRUNCATED,
                        "the requested lines are no longer buffered",
                    ))
                    .await;
                writer.close(CLOSE_NORMAL).await;
                break false;
            }
            Since::Lines(lines) => {
                for (seq, line) in lines {
                    if writer.send_text(&line).await.is_err() {
                        break 'pump false;
                    }
                    sent = seq;
                }
            }
        }
        let now = *progress.borrow_and_update();
        if now.finished && sent >= now.last {
            writer.close(CLOSE_NORMAL).await;
            break true;
        }
        tokio::select! {
            changed = progress.changed() => {
                if changed.is_err() {
                    break false;
                }
            }
            message = inbound.recv() => match message {
                Some(Inbound::Text(text)) => {
                    if let Some(request) = protocol::interrupt_request(&text) {
                        tracing::info!(event = "serve.turn.interrupt", user = %log.key().user, turn = %log.key().turn_id);
                        log.interrupt();
                        if writer.send_text(&protocol::control_success(&request)).await.is_err() {
                            break false;
                        }
                    }
                }
                Some(Inbound::Closed(code)) => {
                    if let Some(code) = code {
                        writer.close(code).await;
                    }
                    break false;
                }
                None => break false,
            },
        }
    };
    reading.abort();
    if delivered {
        log.detach();
        shared.turns.end(log.key());
        return;
    }
    // 断开了：宽限内没人续上就打断（没跑完）或拿掉（跑完了）。
    if let Some(generation) = log.detach() {
        tracing::info!(event = "serve.turn.detached", user = %log.key().user, turn = %log.key().turn_id, sent);
        let shared = shared.clone();
        let task = spawner.spawn("serve-grace", async move {
            tokio::time::sleep(shared.grace).await;
            match log.expire(generation) {
                Expiry::Kept => {}
                Expiry::Interrupt => {
                    tracing::info!(event = "serve.turn.expired", user = %log.key().user, turn = %log.key().turn_id);
                    log.interrupt();
                }
                Expiry::Drop => shared.turns.end(log.key()),
            }
        });
        tasks.keep(task);
    }
}

/// 连接的读：交给发送循环，直到对方关了或读错。
async fn read(mut reader: WsReader, inbox: mpsc::Sender<Inbound>) {
    loop {
        let message = match reader.next().await {
            Ok(WsFrame::Text(text)) => Inbound::Text(text),
            Ok(WsFrame::Binary(_)) => continue,
            Ok(WsFrame::Close { code, .. }) => Inbound::Closed(Some(code)),
            Err(_) => Inbound::Closed(None),
        };
        let closed = matches!(message, Inbound::Closed(_));
        if inbox.send(message).await.is_err() || closed {
            return;
        }
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
