//! 服务器的询问（ext.md §3.13、§3.14）：截下、挂在在途的调用上问、hooks、撤回、sampling 的确认与模型请求。服务器是内存里的
//! 替身，问用户的一方是 kernel-api 的替身。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::TaskScope;
use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::hooks::{
    ElicitationVerdict, HookEvent, HookEventName, HookOutcome, HookRunner,
};
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::interaction::Answer;
use rust_cli_kernel_api::mcp::{McpConnect, McpTransport, McpTransportConfig};
use rust_cli_kernel_api::mcp_ask::{
    ElicitationAction, ElicitationAnswer, ElicitationKind, FieldKind, SamplingAnswer,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
};
use rust_cli_kernel_api::side_request::SideSource;
use rust_cli_kernel_api::testing::{
    FixedClock, ScriptedInteractions, ScriptedModel, text_response, tool_use_context,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::super::connection::REVISIONS;
use super::super::rpc::Client;
use super::*;
use crate::test_support::{FakeMcp, server, workspace};

const ALL: McpClientTraits = McpClientTraits {
    elicitation_form: true,
    elicitation_url: true,
    sampling: true,
};

/// 按事件名给结论的 hooks；记下收到的事件。
#[derive(Default)]
struct Hooks {
    outcomes: Mutex<HashMap<&'static str, HookOutcome>>,
    events: Mutex<Vec<HookEvent>>,
}

impl Hooks {
    fn with(self, name: HookEventName, outcome: HookOutcome) -> Self {
        self.outcomes
            .lock_unpoisoned()
            .insert(name.as_str(), outcome);
        self
    }

    fn names(&self) -> Vec<&'static str> {
        self.events
            .lock_unpoisoned()
            .iter()
            .map(|event| event.name.as_str())
            .collect()
    }
}

#[async_trait]
impl HookRunner for Hooks {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        let outcome = self
            .outcomes
            .lock_unpoisoned()
            .get(event.name.as_str())
            .cloned()
            .unwrap_or_default();
        self.events.lock_unpoisoned().push(event);
        outcome
    }
}

/// 一直不回答的用户：记下撤回的询问。
#[derive(Default)]
struct Waiting {
    asked: Mutex<Vec<InteractionRequest>>,
    withdrawn: Mutex<Vec<ToolUseId>>,
    arrived: tokio::sync::Notify,
}

#[async_trait]
impl Interactions for Waiting {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        self.asked.lock_unpoisoned().push(request);
        self.arrived.notify_waiters();
        std::future::pending().await
    }

    async fn cancel_ask(&self, tool_use: &ToolUseId) {
        self.withdrawn.lock_unpoisoned().push(tool_use.clone());
        self.arrived.notify_waiters();
    }

    async fn answer_ask(&self, _tool_use: &ToolUseId, _answer: Answer) -> bool {
        false
    }

    async fn apply_permission_updates(
        &self,
        _updates: Vec<rust_cli_kernel_api::permission_updates::PermissionUpdate>,
    ) -> Result<(), OrchestrationError> {
        Ok(())
    }

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        None
    }
}

impl Waiting {
    /// 等它收到一个询问或撤回。
    async fn until(&self, done: impl Fn(&Self) -> bool) {
        loop {
            let mut arrived = std::pin::pin!(self.arrived.notified());
            arrived.as_mut().enable();
            if done(self) {
                return;
            }
            arrived.await;
        }
    }
}

struct Rig {
    fake: Arc<FakeMcp>,
    client: Arc<Client>,
    _scope: TaskScope,
}

/// 连上服务器 `s`（工具调用不回答，挂着），带问用户的端口。
async fn connect(traits: McpClientTraits, hooks: Arc<Hooks>, model: Arc<ScriptedModel>) -> Rig {
    let fake =
        Arc::new(FakeMcp::default().with("s", server(json!({"tools": {}}), None, |_, _| None)));
    let transport: Arc<dyn McpTransport> = fake.clone();
    let request = McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::stdio("node", Vec::new()),
    };
    let id = transport.connect(&workspace(), request).await.unwrap();
    let scope = TaskScope::root("test", |_| {});
    let ports = Arc::new(AskPorts::new(
        traits,
        hooks,
        model,
        ModelSelection::new("zai", GLM_5_3).into(),
        Arc::new(FixedClock(1_000)),
    ));
    let opened = Client::connect(
        transport,
        id,
        &scope.spawner(),
        vec!["/Users/dev/project".to_owned()],
        &REVISIONS,
        ("1", Some((&ports, "srv"))),
    )
    .await
    .unwrap();
    Rig {
        fake,
        client: opened.client,
        _scope: scope,
    }
}

impl Rig {
    /// 起一次工具调用（服务器不回答），询问挂在它上面；返回它的取消。
    fn call(&self, interactions: Arc<dyn Interactions>) -> CancellationToken {
        let mut context = tool_use_context();
        context.interactions = interactions;
        let cancel = context.cancel.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            let _ = client
                .request_for(
                    Caller::of(&context),
                    "tools/call",
                    json!({"name": "t"}),
                    (Duration::from_secs(100_000), &context.cancel),
                )
                .await;
        });
        cancel
    }

    fn push(&self, message: Value) {
        self.fake.push("s", message);
    }

    async fn answer(&self, id: i64) -> Value {
        self.fake
            .arrival(|message| message["id"] == id && message.get("method").is_none())
            .await
    }

    /// 工具调用已经到了服务器（询问有了去处）。
    async fn called(&self) {
        self.fake
            .arrival(|message| message["method"] == "tools/call")
            .await;
    }
}

fn form(id: i64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "elicitation/create", "params": {
        "message": "Who are you?",
        "requestedSchema": {"type": "object", "properties": {
            "name": {"type": "string", "title": "Name", "minLength": 2},
            "age": {"type": "integer", "minimum": 0},
            "color": {"type": "string", "enum": ["r", "g"], "enumNames": ["Red", "Green"]}
        }, "required": ["name", "missing"]}
    }})
}

fn accepted(content: Value) -> Answer {
    Answer::McpElicitation(ElicitationAnswer::Accept {
        content: content.as_object().cloned().unwrap(),
    })
}

/// 声明的能力：前端给的两种模式与 sampling；什么都不声明时只有 roots（以前的握手）。
#[tokio::test]
async fn the_handshake_declares_what_the_frontend_supports() {
    let rig = connect(ALL, Arc::default(), Arc::default()).await;
    let initialize = rig
        .fake
        .arrival(|message| message["method"] == "initialize")
        .await;
    assert_eq!(
        initialize["params"]["capabilities"],
        json!({"roots": {}, "sampling": {}, "elicitation": {"form": {}, "url": {}}})
    );
    let none = rig_plain().await;
    let initialize = none
        .fake
        .arrival(|message| message["method"] == "initialize")
        .await;
    assert_eq!(initialize["params"]["capabilities"], json!({"roots": {}}));
}

async fn rig_plain() -> Rig {
    connect(McpClientTraits::default(), Arc::default(), Arc::default()).await
}

/// 在途的工具调用收到表单请求：去处的用户被问到（字段按服务器的顺序、只认在 `properties` 里的必填名），回答原样回给服务器。
#[tokio::test]
async fn a_form_request_is_asked_on_the_calling_tool() {
    let hooks = Arc::new(Hooks::default());
    let rig = connect(ALL, hooks.clone(), Arc::default()).await;
    let user = Arc::new(ScriptedInteractions::always(accepted(
        json!({"name": "dev"}),
    )));
    rig.call(user.clone());
    rig.called().await;
    rig.push(form(5));
    assert_eq!(
        rig.answer(5).await,
        json!({"jsonrpc": "2.0", "id": 5, "result": {"action": "accept", "content": {"name": "dev"}}})
    );
    let asked = user.asked();
    let [
        InteractionRequest::McpElicitation {
            tool_use,
            elicitation,
            ..
        },
    ] = asked.as_slice()
    else {
        panic!("{asked:?}");
    };
    assert_eq!(tool_use.as_str(), "toolu_1");
    assert_eq!(elicitation.server, "srv");
    let ElicitationKind::Form { fields } = &elicitation.kind else {
        panic!("{elicitation:?}");
    };
    let names: Vec<(&str, bool)> = fields
        .iter()
        .map(|f| (f.name.as_str(), f.required))
        .collect();
    assert_eq!(names, [("name", true), ("age", false), ("color", false)]);
    assert!(
        matches!(&fields[2].kind, FieldKind::Choice { options, .. } if options[1].label == "Green")
    );
    // hooks 的顺序（ext.md §3.13.6）：询问之前、回答之后，最后一个通知（没有这个事件的 hook 时不跑，这里的替身都有）
    assert_eq!(hooks.names()[..2], ["Elicitation", "ElicitationResult"]);
}

/// 没声明的方法照旧 Method not found（以前的回答）。
#[tokio::test]
async fn undeclared_methods_stay_method_not_found() {
    let rig = rig_plain().await;
    rig.push(form(5));
    rig.push(json!({"jsonrpc": "2.0", "id": 6, "method": "sampling/createMessage", "params": {"messages": [], "maxTokens": 1}}));
    assert_eq!(rig.answer(5).await["error"]["code"], -32601);
    assert_eq!(rig.answer(6).await["error"]["code"], -32601);
}

/// 没有在途的调用：没有可以挂的询问，回 cancel（sampling 回错误）。
#[tokio::test]
async fn a_request_without_a_caller_is_cancelled() {
    let rig = connect(ALL, Arc::default(), Arc::default()).await;
    rig.push(form(5));
    assert_eq!(rig.answer(5).await["result"], json!({"action": "cancel"}));
    rig.push(json!({"jsonrpc": "2.0", "id": 6, "method": "sampling/createMessage", "params": {"messages": [], "maxTokens": 1}}));
    assert_eq!(
        rig.answer(6).await["error"]["message"],
        "No user can approve this sampling request"
    );
}

/// 服务器取消它的请求：询问撤回，这个请求不回答。
#[tokio::test]
async fn a_server_cancel_withdraws_the_ask() {
    let rig = connect(ALL, Arc::default(), Arc::default()).await;
    let user = Arc::new(Waiting::default());
    rig.call(user.clone());
    rig.called().await;
    rig.push(form(5));
    user.until(|user| !user.asked.lock_unpoisoned().is_empty())
        .await;
    rig.push(
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 5}}),
    );
    user.until(|user| !user.withdrawn.lock_unpoisoned().is_empty())
        .await;
    // 之后的请求照常回答，被取消的那个一直没有回答
    rig.push(json!({"jsonrpc": "2.0", "id": 6, "method": "ping"}));
    rig.answer(6).await;
    assert!(
        !rig.fake
            .received()
            .iter()
            .any(|(_, message)| message["id"] == 5)
    );
}

/// 去处的调用被取消（停下回合）：询问撤回，回 cancel。
#[tokio::test]
async fn a_cancelled_call_answers_cancel() {
    let rig = connect(ALL, Arc::default(), Arc::default()).await;
    let user = Arc::new(Waiting::default());
    let cancel = rig.call(user.clone());
    rig.called().await;
    rig.push(form(5));
    user.until(|user| !user.asked.lock_unpoisoned().is_empty())
        .await;
    cancel.cancel();
    assert_eq!(rig.answer(5).await["result"], json!({"action": "cancel"}));
    assert_eq!(
        user.withdrawn.lock_unpoisoned().as_slice(),
        [ToolUseId::new("toolu_1")]
    );
}

/// 连接关了：询问撤回。
#[tokio::test]
async fn a_closed_connection_withdraws_the_ask() {
    let rig = connect(ALL, Arc::default(), Arc::default()).await;
    let user = Arc::new(Waiting::default());
    rig.call(user.clone());
    rig.called().await;
    rig.push(form(5));
    user.until(|user| !user.asked.lock_unpoisoned().is_empty())
        .await;
    rig.fake.drop_server("s", false);
    user.until(|user| !user.withdrawn.lock_unpoisoned().is_empty())
        .await;
}

/// 询问之前的 hook：结论原样回给服务器、不问；阻塞回 decline。
#[tokio::test]
async fn hooks_answer_or_block_before_asking() {
    let verdict = HookOutcome {
        elicitation: Some(ElicitationVerdict {
            action: ElicitationAction::Accept,
            content: json!({"name": "hook"}).as_object().cloned(),
        }),
        ..HookOutcome::default()
    };
    let hooks = Arc::new(Hooks::default().with(HookEventName::Elicitation, verdict));
    let rig = connect(ALL, hooks.clone(), Arc::default()).await;
    let user = Arc::new(ScriptedInteractions::default());
    rig.call(user.clone());
    rig.called().await;
    rig.push(form(5));
    assert_eq!(
        rig.answer(5).await["result"],
        json!({"action": "accept", "content": {"name": "hook"}})
    );
    assert!(user.asked().is_empty());
    assert_eq!(hooks.names(), ["Elicitation"]);
    let event = hooks.events.lock_unpoisoned()[0].clone();
    assert_eq!(event.payload["mcp_server_name"], "srv");
    assert_eq!(
        event.payload["requested_schema"]["required"],
        json!(["name", "missing"])
    );

    let blocking = HookOutcome {
        blocking_error: Some("no".to_owned()),
        ..HookOutcome::default()
    };
    let hooks = Arc::new(Hooks::default().with(HookEventName::Elicitation, blocking));
    let blocked = connect(ALL, hooks, Arc::default()).await;
    blocked.call(user.clone());
    blocked.called().await;
    blocked.push(form(5));
    assert_eq!(
        blocked.answer(5).await["result"],
        json!({"action": "decline"})
    );
}

/// 回答之后的 hook：换掉动作，没给内容时沿用用户的；阻塞回 decline。
#[tokio::test]
async fn result_hooks_replace_the_answer() {
    let verdict = HookOutcome {
        elicitation: Some(ElicitationVerdict {
            action: ElicitationAction::Accept,
            content: None,
        }),
        ..HookOutcome::default()
    };
    let hooks = Arc::new(Hooks::default().with(HookEventName::ElicitationResult, verdict));
    let rig = connect(ALL, hooks.clone(), Arc::default()).await;
    rig.call(Arc::new(ScriptedInteractions::always(accepted(
        json!({"name": "dev"}),
    ))));
    rig.called().await;
    rig.push(form(5));
    assert_eq!(
        rig.answer(5).await["result"],
        json!({"action": "accept", "content": {"name": "dev"}})
    );
    let event = hooks
        .events
        .lock_unpoisoned()
        .iter()
        .find(|event| event.name == HookEventName::ElicitationResult)
        .cloned()
        .unwrap();
    assert_eq!(
        event.payload,
        json!({"mcp_server_name": "srv", "mode": "form", "action": "accept", "content": {"name": "dev"}})
    );
}

/// 没声明的模式、不合 schema 的请求：`-32602`。
#[tokio::test]
async fn unsupported_modes_and_bad_schemas_are_invalid_params() {
    let traits = McpClientTraits {
        elicitation_url: false,
        ..ALL
    };
    let rig = connect(traits, Arc::default(), Arc::default()).await;
    rig.push(json!({"jsonrpc": "2.0", "id": 5, "method": "elicitation/create", "params": {
        "mode": "url", "message": "Sign in", "url": "https://mcp.example.com/login", "elicitationId": "e1"
    }}));
    assert_eq!(
        rig.answer(5).await["error"],
        json!({"code": -32602, "message": "Client does not support URL-mode elicitation requests"})
    );
    rig.push(json!({"jsonrpc": "2.0", "id": 6, "method": "elicitation/create", "params": {
        "message": "?", "requestedSchema": {"type": "object", "properties": {"x": {"type": "object"}}}
    }}));
    let error = rig.answer(6).await["error"].clone();
    assert_eq!(error["code"], -32602);
}

/// 网址模式：同意之后服务器的完成通知触发 Notification hook（带问它的会话）。
#[tokio::test]
async fn a_url_completion_notifies_the_hooks() {
    let hooks = Arc::new(Hooks::default());
    let rig = connect(ALL, hooks.clone(), Arc::default()).await;
    let user = Arc::new(ScriptedInteractions::always(accepted(json!({}))));
    rig.call(user.clone());
    rig.called().await;
    rig.push(json!({"jsonrpc": "2.0", "id": 5, "method": "elicitation/create", "params": {
        "mode": "url", "message": "Sign in", "url": "https://mcp.example.com/login", "elicitationId": "e1"
    }}));
    assert_eq!(rig.answer(5).await["result"], json!({"action": "accept"}));
    rig.push(json!({"jsonrpc": "2.0", "method": "notifications/elicitation/complete", "params": {"elicitationId": "e1"}}));
    loop {
        let done = hooks.events.lock_unpoisoned().iter().any(|event| {
            event.payload["notification_type"] == "elicitation_complete"
                && event.payload["message"]
                    == "MCP server \"srv\" confirmed elicitation e1 complete"
        });
        if done {
            break;
        }
        tokio::task::yield_now().await;
    }
}

fn sampling(id: i64, max_tokens: u32) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "sampling/createMessage", "params": {
        "messages": [{"role": "user", "content": {"type": "text", "text": "Summarize"}}],
        "systemPrompt": "Be brief", "maxTokens": max_tokens, "temperature": 0.5
    }})
}

/// 同意之后用去处会话的模型发一次旁路请求（截后的上限、服务器的系统提示），结果回给服务器。
#[tokio::test]
async fn sampling_asks_then_calls_the_session_model() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["Short."])]));
    let rig = connect(ALL, Arc::default(), model.clone()).await;
    let user = Arc::new(ScriptedInteractions::always(Answer::McpSampling(
        SamplingAnswer::SamplingAllowed { session: false },
    )));
    rig.call(user.clone());
    rig.called().await;
    rig.push(sampling(5, 50_000));
    assert_eq!(
        rig.answer(5).await["result"],
        json!({"model": GLM_5_3, "stopReason": "endTurn", "role": "assistant", "content": {"type": "text", "text": "Short."}})
    );
    let asked = user.asked();
    let [InteractionRequest::McpSampling { sampling, .. }] = asked.as_slice() else {
        panic!("{asked:?}");
    };
    assert_eq!(sampling.max_tokens, MAX_SAMPLING_TOKENS_FOR_TESTS);
    assert_eq!(sampling.system.as_deref(), Some("Be brief"));
    let requests = model.requests();
    let [request] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(
        request.source,
        rust_cli_kernel_api::model::QuerySource::Side(SideSource::McpSampling)
    );
    assert_eq!(request.max_tokens, MAX_SAMPLING_TOKENS_FOR_TESTS);
    assert_eq!(request.system[0].text, "Be brief");
    assert_eq!(request.temperature, Some(0.5));
    assert!(request.tools.is_empty());
}

const MAX_SAMPLING_TOKENS_FOR_TESTS: u32 = super::super::sampling::MAX_SAMPLING_TOKENS;

/// 「本会话一直允许」之后不再问；拒绝是错误 `-1`。
#[tokio::test]
async fn sampling_allowed_for_the_session_is_not_asked_again() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["a"]),
        text_response(&["b"]),
    ]));
    let rig = connect(ALL, Arc::default(), model).await;
    let user = Arc::new(ScriptedInteractions::new([Answer::McpSampling(
        SamplingAnswer::SamplingAllowed { session: true },
    )]));
    rig.call(user.clone());
    rig.called().await;
    rig.push(sampling(5, 10));
    assert_eq!(rig.answer(5).await["result"]["content"]["text"], "a");
    rig.push(sampling(6, 10));
    assert_eq!(rig.answer(6).await["result"]["content"]["text"], "b");
    assert_eq!(user.asked().len(), 1);

    let denied = connect(ALL, Arc::default(), Arc::default()).await;
    denied.call(Arc::new(ScriptedInteractions::always(Answer::McpSampling(
        SamplingAnswer::SamplingDenied,
    ))));
    denied.called().await;
    denied.push(sampling(5, 10));
    assert_eq!(
        denied.answer(5).await["error"],
        json!({"code": -1, "message": "User rejected sampling request"})
    );
}

/// 太大、太频繁的请求不问，直接回错误。
#[tokio::test]
async fn sampling_limits_refuse_without_asking() {
    let rig = connect(ALL, Arc::default(), Arc::default()).await;
    let user = Arc::new(ScriptedInteractions::always(Answer::McpSampling(
        SamplingAnswer::SamplingDenied,
    )));
    rig.call(user.clone());
    rig.called().await;
    let big = "x".repeat(super::super::sampling::MAX_SAMPLING_CHARS + 1);
    rig.push(json!({"jsonrpc": "2.0", "id": 5, "method": "sampling/createMessage", "params": {
        "messages": [{"role": "user", "content": {"type": "text", "text": big}}], "maxTokens": 10
    }}));
    assert_eq!(
        rig.answer(5).await["error"]["message"],
        "Sampling request is too large"
    );
    assert!(user.asked().is_empty());
    // 同一个会话、同一个服务器：一分钟里第 11 个请求被拒（含被拒绝的；时钟固定，都在同一刻）
    for id in 0..super::super::sampling::MAX_SAMPLING_PER_MINUTE {
        let id = 10 + i64::try_from(id).unwrap();
        rig.push(sampling(id, 10));
        assert_eq!(rig.answer(id).await["error"]["code"], -1);
    }
    rig.push(sampling(99, 10));
    assert_eq!(
        rig.answer(99).await["error"]["message"],
        "Too many sampling requests"
    );
}
