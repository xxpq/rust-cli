//! `mcp.trust.set` 在 actor 里的时序（docs/zcode-protocol-v5/18-mcp-trust.md §12.6、§12.7）：真实进程
//! `zcode-rs app-server --stdio --protocol v5`，临时 home 与仓库。写入在信任工作任务里一条一条做：等锁时链路照常，ACK 按发的顺序回，
//! 活会话最后的提示对应最后一次写入。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、找 node、起真实进程与本地夹具、拿住记录文件的锁"
)]

mod app_host;
mod sse;
mod system_env;

mod project_mcp;

use std::path::PathBuf;

use project_mcp::*;
use serde_json::{Value, json};

/// 写入在等 Node 的锁时，链路照常（18 §12.6）：测试先拿住信任记录的目录锁（Node 的形状，owner 是活着的测试进程，不会被回收），
/// 再发 `mcp.trust.set` 与 `ping`。`ping` 先得到回复；放开锁之后这条命令才被接受。
///
/// 缺陷修复（主会话审查 MCP-TRUST-A）：以前 actor 在这条命令上等写入，锁被别的写者拿着时（最多 8 s）这条链路上全部工作区、全部
/// 会话的请求都排在后面；以前的代码上 `ping` 要等到锁超时、命令被拒之后才有回复。
#[test]
fn a_trust_write_waiting_for_the_lock_does_not_hold_the_link() {
    let place = Place::new();
    place.configure_trust(false);
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    let repo = place.repo.to_string_lossy().into_owned();
    host.open_workspace(&repo);
    let mut lock = place.trust_store().into_os_string();
    lock.push(".lock");
    let lock = PathBuf::from(lock);
    write(
        &lock.join("owner-test.json"),
        &json!({"pid": std::process::id(), "createdAt": 0, "token": "test"}),
    );
    let trust = host.begin(
        "command",
        trust_command(&repo, PROJECT_SHARED, &place.digest(PROJECT_SHARED), true),
    );
    let ping = host.begin("ping", json!({}));
    let first = host.next_reply();
    assert_eq!(first["id"], ping, "{first}");
    std::fs::remove_dir_all(&lock).unwrap();
    let ack = host.next_reply();
    assert_eq!(ack["id"], trust, "{ack}");
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    host.close();
}

/// 背靠背的两条 `mcp.trust.set`（18 §12.7）：一条一条写，ACK 按发的顺序回；活会话最后的提示对应第二次写入。
#[test]
fn back_to_back_trust_commands_leave_the_last_write_in_the_hint() {
    let place = Place::new();
    place.configure_trust(false);
    let fixture = sse::serve(Vec::new());
    let mut host = V5Host::start(&place, &fixture.url);
    host.initialize();
    let repo = place.repo.to_string_lossy().into_owned();
    host.open_workspace(&repo);
    host.create_session_as(&repo, "ses_one", "019a5f3c-7a2b-7c3d-8e4f-0000000000c1");
    host.watch_session(&repo, "ses_one", "one");
    let shared = host.begin(
        "command",
        trust_command(&repo, PROJECT_SHARED, &place.digest(PROJECT_SHARED), true),
    );
    let session = host.begin(
        "command",
        trust_command(&repo, PROJECT_SESSION, &place.digest(PROJECT_SESSION), true),
    );
    let first = host.next_reply();
    let second = host.next_reply();
    assert_eq!(
        (first["id"].clone(), second["id"].clone()),
        (json!(shared), json!(session))
    );
    assert_eq!(first["result"]["kind"], "accepted", "{first}");
    assert_eq!(second["result"]["kind"], "accepted", "{second}");
    let admissions = host.mcp_admissions("one");
    let counts: Vec<Value> = admissions
        .iter()
        .map(|value| value.get("pendingCount").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(counts, [json!(2), json!(1), Value::Null], "{admissions:?}");
    host.close();
}
