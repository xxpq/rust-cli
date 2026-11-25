//! App 专有的模型上下文的对拍（docs/specs/rust-cli/context.md §15.13，APP-CTX）：真实的内核（runtime 的 `LocalKernel`、主循环、
//! 上下文、压缩、`/context`）按各种前端形态跑几轮，模型请求的 system 与 messages、`/context` 的明细与基线逐字节比。
//!
//! 基线在收敛之前的代码上生成（提交 f9e2ef79a6）；收敛之后只改了 [`shape`] 怎样把前端形态装进内核，基线不变。system 块很长、各情形大多相同，按内容
//! 哈希只存一份。路径是 POSIX 的示例值（Gen UI 的根目录要是绝对路径），Windows 上不跑。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_ext::PluginSources;
use rust_cli_ext::plugins::{PluginCandidate, PluginOrigin};
use rust_cli_kernel_api::context_usage::ContextUsageQuery;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
use rust_cli_kernel_api::input_extras::{InputExtras, PageState, Quote};
use rust_cli_kernel_api::model::ModelRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, EndReason, Kernel, KernelEvent, Persistence,
    SessionHandle, TurnOutcome,
};
use rust_cli_kernel_api::session_prompt::{FrontendTraits, SessionPrompt};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, environments,
    selection, text_response,
};
use rust_cli_runtime::{
    ContextConfig, Defaults, ExtConfig, Extensions, LocalKernel, Ports, Surface,
};
use serde_json::{Value, json};

use super::declared::GEN_UI_OUTPUT_ROOT;
use super::{AppFrontend, parse};

/// 基线（收敛之前的代码生成）。
const GOLDEN: &str = rust_cli_base::include_lf!("../../../tests/fixtures/app_context_golden.json");

const OFFICIAL: &str = rust_cli_base::names::OFFICIAL_MARKETPLACE;
const WORK: &str = "/Users/dev/work";
const GEN_UI_ROOT: &str = "/Users/dev/.zcode/visualizations";
const VISUALIZE_DIR: &str =
    "/Users/dev/.zcode/cli/plugins/cache/zcode-plugins-official/visualize/1.0.0";
const DEMO_DIR: &str = "/Users/dev/plugins/demo";
/// App 的 Host 给的那一组（`rustLink.ts` 的 `RUST_V5_APP_SERVER_ARGS`）。
const APP: &str = "typed-mentions,code-comments,file-links,gen-ui";

/// 一种前端形态。
#[derive(Clone, Copy)]
enum Shape {
    /// 交互界面。
    Tui,
    /// `-p`。
    Print,
    /// app-server 带 `--frontend <名字>`；None 是没给 `--frontend`。
    App(Option<&'static str>),
}

fn visualize_id() -> String {
    format!("visualize@{OFFICIAL}")
}

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new(WORK),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24.6.0".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    }
}

/// 两个插件：官方市场的 visualize（要 Gen UI）与 `--plugin-dir` 的 demo（`@` 插件引用它），各带一个 skill。
fn environment() -> Arc<MemoryEnvironment> {
    let skill = |name: &str, description: &str| {
        format!("---\nname: {name}\ndescription: {description}\n---\nUse {name}.")
    };
    Arc::new(
        MemoryEnvironment::new(facts())
            .with_file(
                &format!("{VISUALIZE_DIR}/.zcode-plugin/plugin.json"),
                r#"{"name": "visualize", "skills": "skills"}"#,
            )
            .with_file(
                &format!("{VISUALIZE_DIR}/skills/visualize/SKILL.md"),
                &skill("visualize", "Author interactive views."),
            )
            .with_file(
                &format!("{DEMO_DIR}/.zcode-plugin/plugin.json"),
                r#"{"name": "demo", "skills": "skills"}"#,
            )
            .with_file(
                &format!("{DEMO_DIR}/skills/hello/SKILL.md"),
                &skill("hello", "Say hello."),
            ),
    )
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(WORK),
    }
}

/// 插件来源：visualize 是官方市场的候选（`visualize` 为真时启用），demo 是 `--plugin-dir`。
fn sources(visualize: bool) -> PluginSources {
    PluginSources {
        candidates: vec![PluginCandidate {
            dir: EnvPath::new(VISUALIZE_DIR),
            marketplace: OFFICIAL.to_owned(),
            origin: PluginOrigin::Official,
        }],
        enabled_plugins: BTreeMap::from([(visualize_id(), visualize)]),
        ..PluginSources::default()
    }
}

/// 前端形态装进内核：上下文的形态、会话钉住的输入、扩展（前端撑不起来的插件不加载，前端层的端口按插件集装）。收敛前后只有这里
/// 不同。
async fn shape(
    shape: Shape,
    env: &Arc<MemoryEnvironment>,
    visualize: bool,
) -> (Surface, SessionPrompt, Extensions) {
    let (surface, traits, app) = match shape {
        Shape::Tui => (
            Surface::Interactive,
            FrontendTraits {
                shell_escape: true,
                typed_mentions: true,
            },
            AppFrontend::Absent,
        ),
        Shape::Print => (
            Surface::Print,
            FrontendTraits::default(),
            AppFrontend::Absent,
        ),
        Shape::App(names) => {
            let declared = names.map_or_else(Vec::new, |names| parse(names).unwrap());
            let host_env = [(GEN_UI_OUTPUT_ROOT.to_owned(), GEN_UI_ROOT.to_owned())]
                .into_iter()
                .collect();
            let (traits, app) = AppFrontend::declare(&declared, &host_env).unwrap();
            (Surface::Interactive, traits, app)
        }
    };
    let plugins = rust_cli_ext::plugins::load(
        env.as_ref(),
        &workspace(),
        &[EnvPath::new(DEMO_DIR)],
        (&sources(visualize), &app.unhosted()),
    )
    .await;
    let extensions = Extensions {
        frontend_prompt: Some(app.prompt(&plugins)),
        config: ExtConfig {
            plugins: Arc::new(plugins),
            workspace: Some(workspace()),
            ..ExtConfig::default()
        },
        ..Extensions::none()
    };
    (surface, SessionPrompt::new(None, None, traits), extensions)
}

fn model(replies: usize) -> Arc<ScriptedModel> {
    let responses = (0..replies)
        .map(|index| text_response(&[&format!("<summary>Reply {index}.</summary>")]))
        .collect();
    Arc::new(
        ScriptedModel::new(responses).capabilities(Some(ModelCapabilities {
            supports_mid_conversation_system: true,
            ..ModelCapabilities::default()
        })),
    )
}

async fn kernel(
    (form, visualize): (Shape, bool),
    model: Arc<ScriptedModel>,
    store: &Arc<MemoryStore>,
) -> LocalKernel {
    let env = environment();
    let (surface, prompt, extensions) = shape(form, &env, visualize).await;
    let kernel = LocalKernel::with_context_and_extensions(
        Ports {
            model,
            environments: environments(env),
            store: store.clone(),
            artifacts: store.clone(),
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::DontAsk,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
            additional_directories: Vec::new(),
        },
        ContextConfig::new(surface),
        extensions,
    )
    .with_prompt(prompt);
    kernel.start_extensions();
    kernel
}

/// 一步操作：提交一条输入（正文与它带的材料，装箱：材料比别的变体大得多）或手动压缩。
enum Step {
    Submit(&'static str, Box<InputExtras>),
    Compact,
}

fn plain(text: &'static str) -> Step {
    Step::Submit(text, Box::default())
}

/// App 送来的材料：一段文件选段的引用、一条 IM 回复的引用、一个页面的状态（含要转义的 `<`）。
fn material() -> InputExtras {
    InputExtras {
        quotes: vec![
            Quote {
                text: "fn main() {}".to_owned(),
                path: Some(format!("{WORK}/src/main.rs")),
                sender_name: None,
                sender_id: None,
                message_id: None,
                sent_at_ms: None,
            },
            Quote {
                text: "ship it".to_owned(),
                path: None,
                sender_name: Some("Dev".to_owned()),
                sender_id: Some("u1".to_owned()),
                message_id: Some("m1".to_owned()),
                sent_at_ms: Some(1_700_000_000_000),
            },
        ],
        pages: vec![PageState {
            path: format!("{GEN_UI_ROOT}/view.html"),
            state: json!({"count": 2, "note": "<b>bold</b>"}),
        }],
        ..InputExtras::default()
    }
}

/// 带 `@` 插件引用（App 的输入框写出的链接）、引用与页面状态的输入。
fn referenced() -> Step {
    Step::Submit(
        "Check [Demo](plugin://demo@inline) and the view.",
        Box::new(material()),
    )
}

async fn finish(session: &Arc<dyn SessionHandle>, after: u64) -> u64 {
    let mut events = session.subscribe(Some(after));
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .unwrap()
            .unwrap();
        if let KernelEvent::TurnFinished { outcome, .. } = event.event {
            assert_eq!(outcome, TurnOutcome::Success);
            return event.seq;
        }
    }
}

/// 命令 ID 带阶段号：同一个会话的命令账本跨进程，重用的 ID 会被当成重发。
async fn run(session: &Arc<dyn SessionHandle>, phase: usize, steps: Vec<Step>) {
    let mut after = 0;
    for (index, step) in steps.into_iter().enumerate() {
        let command = match step {
            Step::Submit(text, extras) => Command::Submit {
                text: text.to_owned(),
                extras: *extras,
                delivery: Delivery::Queue,
                target: None,
            },
            Step::Compact => Command::Compact { instructions: None },
        };
        session
            .send(CommandEnvelope {
                id: CommandId::new(format!("p{phase}c{index}")),
                command,
            })
            .await;
        after = finish(session, after).await;
    }
}

fn create() -> CreateSession {
    CreateSession {
        id: Some(SessionId::new("ses_golden")),
        workspace: workspace(),
        selection: selection(),
        mode: PermissionMode::DontAsk,
        plan: false,
        persistence: Persistence::Immediate,
        additional_directories: Vec::new(),
        preferences: Default::default(),
    }
}

/// 一次请求的记录：system 存哈希（内容在 `systems` 里），messages 与工具名原样。
fn record(request: &ModelRequest, systems: &mut BTreeMap<String, Value>) -> Value {
    let system = json!(request.system);
    let key = rust_cli_base::hash::sha256_hex(system.to_string().as_bytes());
    systems.insert(key.clone(), system);
    json!({
        "source": request.source,
        "system": key,
        "messages": request.messages,
        "tools": request.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
    })
}

async fn usage(kernel: &LocalKernel) -> Value {
    let usage = kernel
        .context_usage(ContextUsageQuery {
            session: Some(SessionId::new("ses_golden")),
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::DontAsk,
        })
        .await
        .unwrap();
    json!(usage)
}

/// 一个情形：一个或两个阶段（第二个阶段是另一次启动冷打开同一个会话），每个阶段一种前端形态与它的几步。
struct Case {
    name: &'static str,
    phases: Vec<((Shape, bool), Vec<Step>)>,
}

async fn case(case: Case, systems: &mut BTreeMap<String, Value>) -> Value {
    let store = Arc::new(MemoryStore::default());
    let mut requests = Vec::new();
    let mut usages = Vec::new();
    let mut created = false;
    for (phase, (form, steps)) in case.phases.into_iter().enumerate() {
        let replies = steps
            .iter()
            .map(|step| match step {
                Step::Submit(..) | Step::Compact => 1,
            })
            .sum();
        let model = model(replies);
        let kernel = kernel(form, model.clone(), &store).await;
        if !created {
            kernel.create_session(create()).await.unwrap();
            created = true;
        }
        let session = kernel
            .open_session(&SessionId::new("ses_golden"))
            .await
            .unwrap();
        run(&session, phase, steps).await;
        usages.push(usage(&kernel).await);
        session.close(EndReason::Clear).await;
        requests.extend(
            model
                .requests()
                .iter()
                .map(|request| record(request, systems)),
        );
    }
    json!({"case": case.name, "requests": requests, "context_usage": usages})
}

fn cases() -> Vec<Case> {
    let app = |names| (Shape::App(Some(names)), true);
    vec![
        Case {
            name: "tui",
            phases: vec![((Shape::Tui, true), vec![plain("Hello.")])],
        },
        Case {
            name: "print",
            phases: vec![((Shape::Print, true), vec![plain("Hello.")])],
        },
        Case {
            name: "app_visualize_on",
            phases: vec![(app(APP), vec![referenced(), Step::Compact, plain("Go on.")])],
        },
        Case {
            name: "app_visualize_off",
            phases: vec![((Shape::App(Some(APP)), false), vec![referenced()])],
        },
        Case {
            name: "app_code_comments",
            phases: vec![(app("code-comments"), vec![plain("Review it.")])],
        },
        Case {
            name: "app_file_links",
            phases: vec![(app("file-links"), vec![plain("Open it.")])],
        },
        Case {
            name: "app_gen_ui_only",
            phases: vec![(app("gen-ui"), vec![plain("Draw it.")])],
        },
        Case {
            name: "app_shell_escape",
            phases: vec![(
                app("shell-escape,typed-mentions,code-comments"),
                vec![plain("Hello.")],
            )],
        },
        Case {
            name: "app_undeclared",
            phases: vec![((Shape::App(None), true), vec![referenced()])],
        },
        Case {
            name: "app_then_tui",
            phases: vec![
                (app(APP), vec![plain("Hello.")]),
                ((Shape::Tui, true), vec![plain("Again.")]),
            ],
        },
        Case {
            name: "app_then_print",
            phases: vec![
                (app(APP), vec![plain("Hello.")]),
                ((Shape::Print, true), vec![plain("Again.")]),
            ],
        },
        Case {
            name: "app_visualize_later",
            phases: vec![
                ((Shape::App(Some(APP)), false), vec![plain("Hello.")]),
                (app(APP), vec![plain("Again.")]),
            ],
        },
    ]
}

/// 模型请求与 `/context` 的明细与基线逐字节相同。
#[tokio::test]
#[cfg_attr(windows, ignore = "the golden paths are POSIX examples")]
async fn the_app_context_matches_the_golden() {
    let mut systems = BTreeMap::new();
    let mut runs = Vec::new();
    for item in cases() {
        runs.push(case(item, &mut systems).await);
    }
    let actual =
        serde_json::to_string_pretty(&json!({"cases": runs, "systems": systems})).unwrap() + "\n";
    if actual != GOLDEN {
        let expected: Value = serde_json::from_str(GOLDEN).unwrap_or(Value::Null);
        let actual: Value = serde_json::from_str(&actual).unwrap();
        for (index, run) in actual["cases"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                run, &expected["cases"][index],
                "case {} differs from the golden",
                run["case"]
            );
        }
        assert_eq!(
            actual["systems"], expected["systems"],
            "system blocks differ"
        );
        panic!("the golden differs only in formatting");
    }
}

/// `plugin://` 链接只有 App 解析（context.md §15.11，APP-CTX）：TUI 与 `-p` 的输入里手写的链接当普通文字，没有
/// `plugin_reference` 提醒；同一条输入在 App 里有。收敛之前所有入口都解析，这一条在那时的代码上失败。
#[tokio::test]
#[cfg_attr(windows, ignore = "the golden paths are POSIX examples")]
async fn only_the_app_parses_plugin_links() {
    let reminded = |run: &Value| {
        run["requests"].as_array().unwrap().iter().any(|request| {
            request["messages"]
                .to_string()
                .contains("<plugin_reference>")
        })
    };
    let mut systems = BTreeMap::new();
    for (name, form) in [("tui", Shape::Tui), ("print", Shape::Print)] {
        let run = case(
            Case {
                name,
                phases: vec![((form, true), vec![referenced()])],
            },
            &mut systems,
        )
        .await;
        assert!(!reminded(&run), "{name}: {run}");
    }
    let app = case(
        Case {
            name: "app",
            phases: vec![((Shape::App(Some(APP)), true), vec![referenced()])],
        },
        &mut systems,
    )
    .await;
    assert!(reminded(&app));
}
