//! 插件管理（protocol-v5.md §72.6；24-plugins.md §11）的进程内场景：真实的 actor 与 Content-Length 分帧，端口是
//! `plugin_admin_fake.rs` 的替身。客户端按帧重建每个订阅的状态（同 conformance 执行器的 TopicStore），每收一帧还一格信用。
use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_base::sync::LockUnpoisoned;
use serde_json::{Map, Value, json};

use super::*;
use crate::consts::PLUGIN_OPERATIONS_RETAINED;
use crate::plugin_admin::PluginOp;
use crate::plugin_admin_fake::{FakeAdmin, Gate, Market};
use crate::test_link::Harness;

#[cfg(not(windows))]
const WS: &str = "/Users/dev/app";
#[cfg(windows)]
const WS: &str = r"C:\Users\dev\app";
const SOURCE: &str = "/Users/dev/market";
const MARKET: &str = "local-market";
const PLUGIN: &str = "hello@local-market";
const USER_SUB: &str = "u";
const WS_SUB: &str = "w";
const TOKEN: &str = "s3cret-value";

fn market() -> Market {
    Market {
        name: MARKET.to_owned(),
        plugins: [
            (
                "hello".to_owned(),
                [("region".to_owned(), false), ("token".to_owned(), true)].into(),
            ),
            ("other".to_owned(), BTreeMap::new()),
        ]
        .into(),
    }
}

/// 一个订阅按帧重建的状态。
#[derive(Default)]
struct Model {
    state: Map<String, Value>,
    collections: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Model {
    fn apply(&mut self, body: &Value) {
        if body["kind"] == json!("snapshot") {
            self.state = body["snapshot"]["state"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            self.collections = body["snapshot"]["collections"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(col, items)| {
                    let items = items
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| (item["id"].as_str().unwrap().to_owned(), item.clone()))
                        .collect();
                    (col.clone(), items)
                })
                .collect();
            return;
        }
        for delta in body["deltas"].as_array().unwrap() {
            match delta["op"].as_str().unwrap() {
                "state.set" => {
                    for (key, value) in delta["patch"].as_object().unwrap() {
                        self.state.insert(key.clone(), value.clone());
                    }
                }
                "entity.put" => {
                    let id = delta["id"].as_str().unwrap().to_owned();
                    let mut item = delta["data"].clone();
                    item["id"] = json!(id);
                    self.collections
                        .entry(delta["col"].as_str().unwrap().to_owned())
                        .or_default()
                        .insert(id, item);
                }
                "entity.remove" => {
                    if let Some(items) = self.collections.get_mut(delta["col"].as_str().unwrap()) {
                        items.remove(delta["id"].as_str().unwrap());
                    }
                }
                other => panic!("unexpected op {other}"),
            }
        }
    }

    fn entity(&self, col: &str, id: &str) -> Option<&Value> {
        self.collections.get(col)?.get(id)
    }
}

struct Client {
    h: Harness,
    models: BTreeMap<String, Model>,
    /// 收到的全部消息（帧与回复），按到达的顺序。
    log: Vec<Value>,
    /// 等别的 id 时先到的回复。
    replies: BTreeMap<u64, Value>,
    next: u64,
}

impl Client {
    async fn start(admin: Option<Arc<FakeAdmin>>) -> Self {
        let h = match admin {
            Some(admin) => Harness::with_plugins(admin),
            None => Harness::new(),
        };
        let mut client = Self {
            h,
            models: BTreeMap::new(),
            log: Vec::new(),
            replies: BTreeMap::new(),
            next: 1,
        };
        let reply = client
            .request(
                "initialize",
                json!({"clientId": "t", "caps": {"core": [1], "agent": [1], "catalog.plugins": [1]},
                       "contract": crate::generated::contract::CONTRACT_DIGEST}),
            )
            .await;
        assert!(reply.get("result").is_some(), "{reply}");
        client.log.clear();
        client
    }

    async fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.h
            .send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    /// 读到对这个 id 的回复为止；中间的帧应用进各自订阅的状态并还信用，别的回复留着。
    async fn wait(&mut self, id: u64) -> Value {
        if let Some(reply) = self.replies.remove(&id) {
            return reply;
        }
        loop {
            let message = self.h.next().await;
            self.log.push(message.clone());
            if message["method"] == json!("frame") {
                let sub = message["params"]["sub"].as_str().unwrap().to_owned();
                self.models
                    .entry(sub.clone())
                    .or_default()
                    .apply(&message["params"]["body"]);
                self.h
                    .send(json!({"jsonrpc": "2.0", "method": "credit", "params": {"sub": sub, "n": 1}}))
                    .await;
                continue;
            }
            match message["id"].as_u64() {
                Some(got) if got == id => return message,
                Some(other) => {
                    self.replies.insert(other, message);
                }
                None => {}
            }
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.wait(id).await
    }

    async fn subscribe(&mut self, sub: &str, topic: &str) -> Value {
        self.request("subscribe", json!({"sub": sub, "topic": topic}))
            .await
    }

    async fn command_id(&mut self, command_id: &str, kind: &str, payload: Value) -> u64 {
        let target = json!({"hostId": "local", "workspaceKey": WS});
        self.send(
            "command",
            json!({"commandId": command_id, "type": kind, "target": target, "payload": payload}),
        )
        .await
    }

    /// 发一条命令、等它的回复，回 `result`。
    async fn command(&mut self, command_id: &str, kind: &str, payload: Value) -> Value {
        let id = self.command_id(command_id, kind, payload).await;
        self.wait(id).await["result"].clone()
    }

    async fn ping(&mut self) {
        self.request("ping", json!({})).await;
    }

    fn user(&self) -> &Model {
        &self.models[USER_SUB]
    }

    fn workspace(&self) -> &Model {
        &self.models[WS_SUB]
    }

    /// 订两个 topic、加市场、装 hello。
    async fn installed(admin: &Arc<FakeAdmin>) -> Self {
        let mut client = Self::start(Some(admin.clone())).await;
        client.subscribe(USER_SUB, PLUGINS_TOPIC).await;
        client
            .subscribe(WS_SUB, &workspace_topic(WS, WORKSPACE_PLUGINS))
            .await;
        let added = client
            .command(
                "c-add",
                "plugins.marketplace.add",
                json!({"source": SOURCE}),
            )
            .await;
        assert_eq!(added["kind"], json!("accepted"), "{added}");
        let installed = client
            .command("c-install", "plugins.install", json!({"pluginId": PLUGIN}))
            .await;
        assert_eq!(installed["kind"], json!("accepted"), "{installed}");
        client
    }
}

fn admin() -> Arc<FakeAdmin> {
    Arc::new(FakeAdmin::with_source(SOURCE, market()))
}

/// 这条操作在帧里出现过的阶段（按顺序，去掉相邻的重复）。
fn phases(log: &[Value], command_id: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for message in log {
        let body = &message["params"]["body"];
        let deltas = body["deltas"].as_array().cloned().unwrap_or_default();
        for delta in deltas {
            if delta["col"] == json!(OPERATIONS) && delta["id"] == json!(command_id) {
                let phase = delta["data"]["phase"]
                    .as_str()
                    .unwrap_or("removed")
                    .to_owned();
                if seen.last() != Some(&phase) {
                    seen.push(phase);
                }
            }
        }
    }
    seen
}

#[tokio::test]
async fn without_a_port_the_capability_topics_and_commands_are_absent() {
    let mut client = Client::start(None).await;
    let failed = client.subscribe(USER_SUB, PLUGINS_TOPIC).await;
    assert_eq!(
        failed["error"]["data"]["code"],
        json!("precondition.notFound"),
        "{failed}"
    );
    let failed = client
        .subscribe(WS_SUB, &workspace_topic(WS, WORKSPACE_PLUGINS))
        .await;
    assert_eq!(
        failed["error"]["data"]["code"],
        json!("precondition.notFound"),
        "{failed}"
    );
    let id = client
        .command_id("c1", "plugins.install", json!({"pluginId": PLUGIN}))
        .await;
    let reply = client.wait(id).await;
    assert_eq!(
        reply["error"]["data"]["code"],
        json!("query.unsupported"),
        "{reply}"
    );
}

#[tokio::test]
async fn the_capability_is_declared_only_with_a_port() {
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                  "params": {"clientId": "t", "caps": {"core": [1], "agent": [1], "catalog.plugins": [1]}}});
    let mut h = Harness::with_plugins(admin());
    h.send(initialize.clone()).await;
    let with = h.next().await;
    assert_eq!(
        with["result"]["caps"]["catalog.plugins"],
        json!(1),
        "{with}"
    );
    let mut h = Harness::new();
    h.send(initialize).await;
    let without = h.next().await;
    assert!(
        without["result"]["caps"].get("catalog.plugins").is_none(),
        "{without}"
    );
}

#[tokio::test]
async fn install_shows_the_operation_until_its_final_phase_and_acks_after_the_effects() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    assert_eq!(
        phases(&client.log, "c-install"),
        ["running", "succeeded"],
        "{:#?}",
        client.log
    );
    let plugin = client.user().entity(PLUGINS, PLUGIN).unwrap();
    assert_eq!(plugin["enabled"], json!(true));
    assert_eq!(plugin["user"]["enabled"], json!(true));
    assert_eq!(
        client.user().entity("catalog", PLUGIN).unwrap()["installed"],
        json!(true)
    );
    assert_eq!(
        client.workspace().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(true)
    );
    // 终态留在 topic 里（24 §6）
    assert_eq!(
        client.user().entity(OPERATIONS, "c-install").unwrap()["phase"],
        json!("succeeded")
    );
    // ACK 排在效果帧之后：安装的回复（最后一条消息之前的最后一个回复）之前最后一帧的位置就是 effects 给的位置
    let ack_at = client
        .log
        .iter()
        .rposition(|m| m["result"]["kind"] == json!("accepted"))
        .unwrap();
    let ack = &client.log[ack_at];
    let effect = ack["result"]["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["topic"] == json!(PLUGINS_TOPIC))
        .unwrap()
        .clone();
    let last_frame = client.log[..ack_at]
        .iter()
        .rev()
        .find(|m| m["params"]["topic"] == json!(PLUGINS_TOPIC))
        .unwrap();
    assert_eq!(last_frame["params"]["to"], effect["seq"], "{ack}");
    // 装已装的：noop
    let again = client
        .command(
            "c-install-2",
            "plugins.install",
            json!({"pluginId": PLUGIN}),
        )
        .await;
    assert_eq!(again["noop"], json!(true), "{again}");
}

#[tokio::test]
async fn set_enabled_moves_the_user_and_workspace_values_and_null_inherits() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    let user_off = client
        .command(
            "c-1",
            "plugins.setEnabled",
            json!({"pluginId": PLUGIN, "scope": "user", "enabled": false}),
        )
        .await;
    assert_eq!(user_off["kind"], json!("accepted"), "{user_off}");
    assert_eq!(
        client.user().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
    // 用户层一改，已订的工作区的生效值也跟着变
    assert_eq!(
        client.workspace().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
    client
        .command(
            "c-2",
            "plugins.setEnabled",
            json!({"pluginId": PLUGIN, "scope": "workspace", "enabled": true}),
        )
        .await;
    let workspace = client.workspace().entity(PLUGINS, PLUGIN).unwrap();
    assert_eq!(workspace["enabled"], json!(true));
    assert_eq!(workspace["workspace"]["enabled"], json!(true));
    assert_eq!(
        client.user().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
    client
        .command(
            "c-3",
            "plugins.setEnabled",
            json!({"pluginId": PLUGIN, "scope": "workspace", "enabled": null}),
        )
        .await;
    let workspace = client.workspace().entity(PLUGINS, PLUGIN).unwrap();
    assert!(
        workspace["workspace"].get("enabled").is_none(),
        "{workspace}"
    );
    assert_eq!(workspace["enabled"], json!(false));
    // 工作区层要 target.workspaceKey
    let id = client
        .send(
            "command",
            json!({"commandId": "c-4", "type": "plugins.setEnabled", "target": {"hostId": "local"},
                   "payload": {"pluginId": PLUGIN, "scope": "workspace", "enabled": true}}),
        )
        .await;
    let refused = client.wait(id).await;
    assert_eq!(
        refused["result"]["code"],
        json!("precondition.workspaceRequired"),
        "{refused}"
    );
    // 不认识的插件
    let unknown = client
        .command(
            "c-5",
            "plugins.setEnabled",
            json!({"pluginId": "nope@local-market", "scope": "user", "enabled": true}),
        )
        .await;
    assert_eq!(
        unknown["code"],
        json!("precondition.pluginNotFound"),
        "{unknown}"
    );
    assert_eq!(
        client.user().entity(OPERATIONS, "c-5").unwrap()["error"]["code"],
        json!("precondition.pluginNotFound")
    );
}

#[tokio::test]
async fn configure_never_echoes_a_secret() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    let ack = client
        .command(
            "c-1",
            "plugins.configure",
            json!({"pluginId": PLUGIN, "scope": "user", "set": {"region": "eu", "token": TOKEN}}),
        )
        .await;
    assert_eq!(ack["kind"], json!("accepted"), "{ack}");
    let user = &client.user().entity(PLUGINS, PLUGIN).unwrap()["user"];
    assert_eq!(user["options"], json!({"region": "eu"}));
    assert_eq!(user["secretKeys"], json!(["token"]));
    for message in &client.log {
        assert!(!message.to_string().contains(TOKEN), "{message}");
    }
    let wrong = client
        .command(
            "c-2",
            "plugins.configure",
            json!({"pluginId": PLUGIN, "scope": "user", "set": {"missing": 1}}),
        )
        .await;
    assert_eq!(
        wrong["code"],
        json!("precondition.pluginOptionInvalid"),
        "{wrong}"
    );
    assert_eq!(wrong["field"], json!("missing"));
    // 参数严格：多出的字段带路径拒绝，不进操作表
    let strict = client
        .command(
            "c-3",
            "plugins.configure",
            json!({"pluginId": PLUGIN, "scope": "user", "options": {}}),
        )
        .await;
    assert_eq!(
        strict["code"],
        json!("precondition.invalidPayload"),
        "{strict}"
    );
    assert!(client.user().entity(OPERATIONS, "c-3").is_none());
}

#[tokio::test]
async fn uninstall_removes_the_plugin_and_twice_is_a_noop() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    client
        .command("c-1", "plugins.uninstall", json!({"pluginId": PLUGIN}))
        .await;
    assert!(client.user().entity(PLUGINS, PLUGIN).is_none());
    assert!(client.workspace().entity(PLUGINS, PLUGIN).is_none());
    assert_eq!(
        client.user().entity("catalog", PLUGIN).unwrap()["installed"],
        json!(false)
    );
    let again = client
        .command("c-2", "plugins.uninstall", json!({"pluginId": PLUGIN}))
        .await;
    assert_eq!(again["noop"], json!(true), "{again}");
}

#[tokio::test]
async fn cancel_stops_a_running_write_before_the_commit_and_a_waiting_one_without_the_port() {
    let admin = admin();
    let mut client = Client::start(Some(admin.clone())).await;
    client.subscribe(USER_SUB, PLUGINS_TOPIC).await;
    client
        .command(
            "c-add",
            "plugins.marketplace.add",
            json!({"source": SOURCE}),
        )
        .await;
    let gate = Arc::new(Gate::default());
    *admin.write_gate.lock_unpoisoned() = Some(gate.clone());
    let install = client
        .command_id("c-install", "plugins.install", json!({"pluginId": PLUGIN}))
        .await;
    gate.entered.notified().await;
    let queued = client
        .command_id(
            "c-queued",
            "plugins.install",
            json!({"pluginId": "other@local-market"}),
        )
        .await;
    // 写挡住时链路照常（actor 不等，§72.5）
    client.ping().await;
    assert_eq!(
        client.user().entity(OPERATIONS, "c-queued").unwrap()["phase"],
        json!("waiting")
    );
    let cancel = client
        .command(
            "c-cancel",
            "plugins.cancel",
            json!({"commandId": "c-queued"}),
        )
        .await;
    assert_eq!(cancel["result"]["cancelled"], json!(true), "{cancel}");
    let queued = client.wait(queued).await;
    assert_eq!(
        queued["result"]["code"],
        json!("precondition.pluginOperationCancelled"),
        "{queued}"
    );
    let cancel = client
        .command(
            "c-cancel-2",
            "plugins.cancel",
            json!({"commandId": "c-install"}),
        )
        .await;
    assert_eq!(cancel["result"]["cancelled"], json!(true), "{cancel}");
    let install = client.wait(install).await;
    assert_eq!(
        install["result"]["code"],
        json!("precondition.pluginOperationCancelled"),
        "{install}"
    );
    assert_eq!(
        phases(&client.log, "c-install"),
        ["running", "cancelling", "cancelled"]
    );
    assert_eq!(phases(&client.log, "c-queued"), ["waiting", "cancelled"]);
    // 等待中被取消的没有调端口；进行中的停在提交点之前，磁盘没变
    let applied = admin.applied.lock_unpoisoned().clone();
    assert!(
        !applied
            .iter()
            .any(|op| matches!(op, PluginOp::Install(i) if i.plugin_id == "other@local-market")),
        "{applied:?}"
    );
    assert!(client.user().entity(PLUGINS, PLUGIN).is_none());
    // 已到终态的、不认识的：noop
    let late = client
        .command(
            "c-cancel-3",
            "plugins.cancel",
            json!({"commandId": "c-install"}),
        )
        .await;
    assert_eq!(late["noop"], json!(true), "{late}");
    assert_eq!(late["result"]["cancelled"], json!(false));
}

#[tokio::test]
async fn a_toggle_of_another_plugin_does_not_wait_for_a_long_install() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    let gate = Arc::new(Gate::default());
    *admin.write_gate.lock_unpoisoned() = Some(gate.clone());
    let install = client
        .command_id(
            "c-other",
            "plugins.install",
            json!({"pluginId": "other@local-market"}),
        )
        .await;
    gate.entered.notified().await;
    // 别的插件的启停：不碰正在装的插件、不碰安装集合，当场做完（24 §6.1）
    let toggle = client
        .command(
            "c-toggle",
            "plugins.setEnabled",
            json!({"pluginId": PLUGIN, "scope": "user", "enabled": false}),
        )
        .await;
    assert_eq!(toggle["kind"], json!("accepted"), "{toggle}");
    assert_eq!(
        client.user().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
    // 碰正在装的插件、碰安装集合的：等它到终态
    let same = client
        .command_id(
            "c-same",
            "plugins.setEnabled",
            json!({"pluginId": "other@local-market", "scope": "user", "enabled": false}),
        )
        .await;
    let uninstall = client
        .command_id(
            "c-uninstall",
            "plugins.uninstall",
            json!({"pluginId": PLUGIN}),
        )
        .await;
    client.ping().await;
    for id in ["c-same", "c-uninstall"] {
        assert_eq!(
            client.user().entity(OPERATIONS, id).unwrap()["phase"],
            json!("waiting"),
            "{id}"
        );
    }
    assert_eq!(
        client.user().entity(OPERATIONS, "c-other").unwrap()["phase"],
        json!("running")
    );
    gate.release.notify_one();
    let install = client.wait(install).await;
    assert_eq!(install["result"]["kind"], json!("accepted"), "{install}");
    // 后到的看到先到的效果：装上之后才停用它，所以停用成功
    let same = client.wait(same).await;
    assert_eq!(same["result"]["kind"], json!("accepted"), "{same}");
    let uninstall = client.wait(uninstall).await;
    assert_eq!(
        uninstall["result"]["kind"],
        json!("accepted"),
        "{uninstall}"
    );
    assert_eq!(
        client.user().entity(PLUGINS, "other@local-market").unwrap()["enabled"],
        json!(false)
    );
    assert!(client.user().entity(PLUGINS, PLUGIN).is_none());
    assert_eq!(
        phases(&client.log, "c-same"),
        ["waiting", "running", "succeeded"]
    );
    // ACK 的先后：装 → 同一个插件的停用（等装完才开始）
    let position = |id: u64| {
        client
            .log
            .iter()
            .position(|m| m["id"].as_u64() == Some(id))
            .unwrap()
    };
    assert!(position(install["id"].as_u64().unwrap()) < position(same["id"].as_u64().unwrap()));
}

#[tokio::test]
async fn rescan_shows_what_another_process_wrote() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    admin.write_elsewhere(PLUGIN, false);
    let ack = client.command("c-1", "plugins.rescan", json!({})).await;
    assert_eq!(ack["kind"], json!("accepted"), "{ack}");
    assert_eq!(
        client.user().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
    assert_eq!(
        client.workspace().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
}

#[tokio::test]
async fn an_older_read_that_arrives_late_does_not_overwrite_a_newer_one() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    let gate = Arc::new(Gate::default());
    *admin.read_gate.lock_unpoisoned() = Some(gate.clone());
    // 第一次 rescan：读到的是旧的（用户层 true），挡在交出之前
    let old = client
        .command_id("c-old", "plugins.rescan", json!({}))
        .await;
    gate.entered.notified().await;
    *admin.read_gate.lock_unpoisoned() = None;
    admin.write_elsewhere(PLUGIN, false);
    client.command("c-new", "plugins.rescan", json!({})).await;
    assert_eq!(
        client.user().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
    gate.release.notify_one();
    client.wait(old).await;
    // 晚到的旧读被丢掉：值没有退回去
    assert_eq!(
        client.user().entity(PLUGINS, PLUGIN).unwrap()["enabled"],
        json!(false)
    );
}

#[tokio::test]
async fn the_same_command_id_again_is_a_duplicate() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    let again = client
        .command("c-install", "plugins.install", json!({"pluginId": PLUGIN}))
        .await;
    assert_eq!(again["kind"], json!("duplicate"), "{again}");
    assert_eq!(again["original"]["kind"], json!("accepted"));
}

#[tokio::test]
async fn final_operations_are_kept_up_to_the_bound() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    // 已有两条终态（加市场、安装）；再做到超出上限一条
    for n in 0..PLUGIN_OPERATIONS_RETAINED - 1 {
        client
            .command(
                &format!("c-{n}"),
                "plugins.setEnabled",
                json!({"pluginId": PLUGIN, "scope": "user", "enabled": n % 2 == 0}),
            )
            .await;
    }
    let operations = &client.user().collections[OPERATIONS];
    assert_eq!(operations.len(), PLUGIN_OPERATIONS_RETAINED);
    assert!(
        !operations.contains_key("c-add"),
        "the oldest final one is removed"
    );
    assert!(operations.contains_key("c-install"));
}

#[tokio::test]
async fn describe_reads_the_local_market_only() {
    let admin = admin();
    let mut client = Client::installed(&admin).await;
    let query = |plugin_id: &str| json!({"type": "plugins.describe", "target": {"hostId": "local"}, "params": {"pluginId": plugin_id}});
    let found = client.request("query", query("other@local-market")).await;
    assert_eq!(
        found["result"]["data"]["components"][0]["items"][0]["name"],
        json!("other"),
        "{found}"
    );
    let missing = client.request("query", query("nope@local-market")).await;
    assert_eq!(
        missing["error"]["data"]["code"],
        json!("precondition.pluginNotFound"),
        "{missing}"
    );
}

#[tokio::test]
async fn shutdown_cancels_the_running_write_and_answers_after_it() {
    let admin = admin();
    let mut client = Client::start(Some(admin.clone())).await;
    client.subscribe(USER_SUB, PLUGINS_TOPIC).await;
    client
        .command(
            "c-add",
            "plugins.marketplace.add",
            json!({"source": SOURCE}),
        )
        .await;
    let gate = Arc::new(Gate::default());
    *admin.write_gate.lock_unpoisoned() = Some(gate.clone());
    let install = client
        .command_id("c-install", "plugins.install", json!({"pluginId": PLUGIN}))
        .await;
    gate.entered.notified().await;
    let shutdown = client.send("shutdown", json!({})).await;
    let install = client.wait(install).await;
    assert_eq!(
        install["result"]["code"],
        json!("precondition.pluginOperationCancelled"),
        "{install}"
    );
    let shutdown = client.wait(shutdown).await;
    assert!(shutdown.get("result").is_some(), "{shutdown}");
}

#[test]
fn the_schemas_compile() {
    let compiled = &crate::plugin_ops::schemas().commands;
    for kind in [
        "plugins.install",
        "plugins.uninstall",
        "plugins.update",
        "plugins.setEnabled",
        "plugins.configure",
        "plugins.cancel",
        "plugins.rescan",
        "plugins.marketplace.add",
        "plugins.marketplace.remove",
        "plugins.marketplace.update",
    ] {
        assert!(compiled.contains_key(kind), "{kind}");
    }
    assert!(crate::plugin_ops::schemas().describe.is_some());
}
