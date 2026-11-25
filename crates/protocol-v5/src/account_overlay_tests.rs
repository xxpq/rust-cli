//! `account.overlay.set`（22-account-overlay.md §3.3、§3.5）：进程级的设值命令，写进外部账号层的格子；同内容是 noop；参数严格。
use rust_cli_base::models::GLM_5_2_ACCOUNT;
use rust_cli_kernel_api::external_accounts::{AccountGrant, AccountGrants, AccountLayer};
use serde_json::{Value, json};

use crate::test_link::{Harness, initialize};

fn command(id: u64, target: Value, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
        "commandId": format!("c{id}"), "type": "account.overlay.set", "target": target, "payload": payload,
    }})
}

fn host() -> Value {
    json!({"hostId": "local"})
}

/// Host 算账号层时读的内置配置的摘要（测试值，22 §15.1）。
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn start_plan() -> Value {
    json!({"builtinConfigDigest": DIGEST, "providers": {
        "account:zai-start-plan": {"entitled": true, "current": true, "models": [GLM_5_2_ACCOUNT]},
        "account:zai-highspeed-card": {"entitled": false},
    }})
}

/// 握手之后（进程级部件还没建好也行：它不依赖组装），发一条命令，回它的结果。
async fn handshaken() -> Harness {
    let mut h = Harness::new();
    h.send(initialize()).await;
    h.next().await;
    h
}

#[tokio::test]
async fn the_layer_lands_in_the_cell_and_the_same_layer_is_a_noop() {
    let mut h = handshaken().await;
    let accounts = h.handle.accounts();
    assert_eq!(accounts.current(), None);
    h.send(command(2, host(), start_plan())).await;
    let ack = h.reply(2, &mut Vec::new()).await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    assert_eq!(ack["result"]["noop"], false);
    // 没有 topic 的效果
    assert_eq!(ack["result"]["effects"], json!([]));
    let facts = accounts.current().unwrap();
    assert_eq!(facts.version, 1);
    assert_eq!(
        *facts.layer,
        AccountLayer {
            builtin_digest: DIGEST.to_owned(),
            grants: AccountGrants::from([
                (
                    "account:zai-highspeed-card".to_owned(),
                    AccountGrant {
                        entitled: false,
                        current: None,
                        models: None,
                    },
                ),
                (
                    "account:zai-start-plan".to_owned(),
                    AccountGrant {
                        entitled: true,
                        current: Some(true),
                        models: Some(vec![GLM_5_2_ACCOUNT.to_owned()]),
                    },
                ),
            ]),
        }
    );
    h.send(command(3, host(), start_plan())).await;
    let again = h.reply(3, &mut Vec::new()).await;
    assert_eq!(again["result"]["noop"], true, "{again}");
    assert_eq!(accounts.current().unwrap().version, 1);
    // 同样的账号事实、依据的内置配置换了：不是 noop（平台层要重新比，22 §15.4）
    let mut moved = start_plan();
    moved["builtinConfigDigest"] = json!("f".repeat(DIGEST.len()));
    h.send(command(4, host(), moved)).await;
    let next = h.reply(4, &mut Vec::new()).await;
    assert_eq!(next["result"]["noop"], false, "{next}");
    assert_eq!(accounts.current().unwrap().version, 2);
}

#[tokio::test]
async fn the_payload_is_strict() {
    let mut h = handshaken().await;
    let accounts = h.handle.accounts();
    h.send(command(
        2,
        host(),
        json!({"builtinConfigDigest": DIGEST, "providers": {"account:zai-start-plan": {"entitled": true, "availability": "available"}}}),
    ))
    .await;
    let extra = h.reply(2, &mut Vec::new()).await;
    assert_eq!(extra["result"]["kind"], "rejected", "{extra}");
    assert_eq!(extra["result"]["code"], "precondition.invalidPayload");
    h.send(command(
        3,
        host(),
        json!({"builtinConfigDigest": DIGEST, "providers": {"account:zai-start-plan": {"current": true}}}),
    ))
    .await;
    let missing = h.reply(3, &mut Vec::new()).await;
    assert_eq!(
        missing["result"]["code"], "precondition.invalidPayload",
        "{missing}"
    );
    // 内置配置摘要必填，64 位小写十六进制（22 §15.3）
    for (id, payload) in [
        (4, json!({"providers": {}})),
        (
            5,
            json!({"builtinConfigDigest": DIGEST.to_uppercase(), "providers": {}}),
        ),
        (
            6,
            json!({"builtinConfigDigest": &DIGEST[1..], "providers": {}}),
        ),
    ] {
        h.send(command(id, host(), payload)).await;
        let bad = h.reply(id, &mut Vec::new()).await;
        assert_eq!(
            bad["result"]["code"], "precondition.invalidPayload",
            "{bad}"
        );
        assert_eq!(bad["result"]["field"], "builtinConfigDigest", "{bad}");
    }
    // 被拒的不进格子
    assert_eq!(accounts.current(), None);
}

/// 握手没协商出 account.overlay（这个 Host 不推账号层）：格子标为「不会来」，等第一份的组装不再等；协商出了就一直等到 Host 推。
#[tokio::test]
async fn a_host_without_the_capability_declines_the_layer() {
    let without = handshaken().await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        without.handle.accounts().arrived(),
    )
    .await
    .expect("declined at the handshake");
    assert_eq!(without.handle.accounts().current(), None);

    let mut with = Harness::new();
    with.send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "clientId": "t", "caps": {"core": [1], "agent": [1], "account.overlay": [1]},
            "contract": crate::generated::contract::CONTRACT_DIGEST,
        }}),
    )
    .await;
    with.next().await;
    let accounts = with.handle.accounts();
    let waiting = tokio::spawn(async move { accounts.arrived().await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "waits for the host's first copy");
    with.send(command(2, host(), start_plan())).await;
    with.reply(2, &mut Vec::new()).await;
    waiting.await.unwrap();
}
