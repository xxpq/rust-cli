//! auto 生效时的分流（docs/specs/rust-cli/auto-classifier.md §3.3、§16.2）：哪些询问问人、哪些交分类器。

use std::sync::Mutex;

use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::mcp::McpToolInfo;

use super::*;

/// 记下交给它的工具名与用户规则，回固定的结论。
struct Judge {
    verdict: ClassifierVerdict,
    asked: Mutex<Vec<String>>,
    rules: Mutex<Vec<(Vec<String>, Vec<String>)>>,
}

impl Judge {
    fn new(verdict: ClassifierVerdict) -> Arc<Self> {
        Arc::new(Self {
            verdict,
            asked: Mutex::default(),
            rules: Mutex::default(),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock_unpoisoned().clone()
    }
}

#[async_trait]
impl AutoClassifier for Judge {
    async fn classify(&self, request: ClassifyRequest<'_>) -> ClassifierVerdict {
        self.asked
            .lock_unpoisoned()
            .push(request.tool.name().to_owned());
        self.rules.lock_unpoisoned().push((
            request.rules.auto_mode.allow.clone(),
            request.rules.deny.to_vec(),
        ));
        self.verdict.clone()
    }
}

/// 一个 MCP 工具：只读与否由测试给。
struct Mcp {
    info: McpToolInfo,
    read_only: bool,
}

impl Mcp {
    fn new(read_only: bool) -> Self {
        Self {
            info: McpToolInfo {
                server: "files".to_owned(),
                tool: "write".to_owned(),
                always_load: false,
            },
            read_only,
        }
    }
}

#[async_trait]
impl Tool for Mcp {
    fn classifier_input(&self, _input: &Value) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }
    fn name(&self) -> &str {
        "mcp__files__write"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        String::new()
    }
    fn is_read_only(&self, _input: &Value) -> bool {
        self.read_only
    }
    fn mcp_info(&self) -> Option<&McpToolInfo> {
        Some(&self.info)
    }
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Passthrough
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Unsupported {
            what: "test".to_owned(),
        })
    }
}

/// plan 缺省开自动语义（`useAutoModeDuringPlan`）：只换上分类器。
fn planned(gate: PolicyGate, judge: &Arc<Judge>) -> PolicyGate {
    gate.with_classifier(judge.clone())
}

/// ask 规则命中时问人，不交分类器（对照实现的 `ask_rule`）。修前：交分类器，分类器放行就执行了。
#[tokio::test]
async fn an_ask_rule_asks_the_user_in_auto_mode() {
    let judge = Judge::new(ClassifierVerdict::Allow);
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Ask, "Bash(git push:*)")]))
        .with_classifier(judge.clone());
    assert!(matches!(
        bash(&gate, PermissionMode::Auto, "git push").await,
        PermissionDecision::Ask { .. }
    ));
    let whole =
        PolicyGate::new(rules(&[(RuleBehavior::Ask, "Bash")])).with_classifier(judge.clone());
    assert!(matches!(
        bash(&whole, PermissionMode::Auto, "git push").await,
        PermissionDecision::Ask { .. }
    ));
    assert!(judge.asked().is_empty());
}

/// 危险删除问人（对照实现的 `safety_check`，不归分类器）。
#[tokio::test]
async fn a_dangerous_removal_asks_the_user_in_auto_mode() {
    let judge = Judge::new(ClassifierVerdict::Allow);
    let gate = PolicyGate::default().with_classifier(judge.clone());
    assert!(matches!(
        bash(&gate, PermissionMode::Auto, "rm -rf /").await,
        PermissionDecision::Ask { .. }
    ));
    assert!(judge.asked().is_empty());
}

/// plan 本身的写入询问问人（对照实现的 `plan_mode_floor`），文字照对照实现。修前：交分类器。
#[tokio::test]
async fn plan_mode_writes_ask_the_user() {
    let judge = Judge::new(ClassifierVerdict::Allow);
    let gate = planned(PolicyGate::default(), &judge);
    assert_eq!(
        decide(
            &gate,
            PermissionMode::Plan,
            "Write",
            json!({"file_path": "/w/src/a.rs"})
        )
        .await,
        PermissionDecision::ask(
            "Cannot write to /w/src/a.rs while in plan mode.",
            Vec::new()
        )
    );
    assert!(judge.asked().is_empty());
}

/// plan 里的写入询问不被整个工具的 allow 规则盖过（对照实现在 allow 规则之前返回 plan 的询问）。修前：`Write` 的 allow 规则直接放行。
#[tokio::test]
async fn whole_tool_allow_rules_do_not_override_plan_mode_writes() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Write")]));
    assert!(matches!(
        decide(
            &gate,
            PermissionMode::Plan,
            "Write",
            json!({"file_path": "/w/src/a.rs"})
        )
        .await,
        PermissionDecision::Ask { .. }
    ));
}

/// plan 里不是只读的 MCP 工具：`Cannot call <工具> while in plan mode.`，问人，allow 规则盖不过。修前：通用的询问文字，交分类器，allow 规则放行。
#[tokio::test]
async fn plan_mode_asks_before_a_mutating_mcp_tool() {
    let judge = Judge::new(ClassifierVerdict::Allow);
    let gate = planned(
        PolicyGate::new(rules(&[(RuleBehavior::Allow, "mcp__files")])),
        &judge,
    );
    let decided = gate
        .decide(&Mcp::new(false), &json!({}), &context(PermissionMode::Plan))
        .await;
    assert_eq!(
        decided,
        PermissionDecision::ask(
            "Cannot call mcp__files__write while in plan mode.",
            Vec::new()
        )
    );
    assert!(judge.asked().is_empty());
    // 只读的 MCP 工具照 allow 规则放行；没有规则时的询问交分类器。
    assert_eq!(
        gate.decide(&Mcp::new(true), &json!({}), &context(PermissionMode::Plan))
            .await,
        allow()
    );
    let bare = planned(PolicyGate::default(), &judge);
    assert_eq!(
        bare.decide(&Mcp::new(true), &json!({}), &context(PermissionMode::Plan))
            .await,
        allow()
    );
    assert_eq!(judge.asked(), ["mcp__files__write"]);
}

/// 敏感文件这类安全检查交分类器（对照实现：`classifierApprovable`），不做 acceptEdits 模拟。修前：问人，不交分类器。
#[tokio::test]
async fn sensitive_files_go_to_the_classifier_in_auto_mode() {
    let judge = Judge::new(ClassifierVerdict::Block {
        reason: "Self-Modification".to_owned(),
    });
    let gate = PolicyGate::default().with_classifier(judge.clone());
    assert_eq!(
        decide(
            &gate,
            PermissionMode::Auto,
            "Write",
            json!({"file_path": "/w/.zcode/settings.json"})
        )
        .await,
        deny(texts::classifier_blocked("Self-Modification", None))
    );
    // 后台 `&` 同样交分类器（修前问人）。
    assert_eq!(
        bash(&gate, PermissionMode::Auto, "sleep 1 &").await,
        deny(texts::classifier_blocked("Self-Modification", None))
    );
    assert_eq!(judge.asked(), ["Write", "Bash"]);
}

/// 交给分类器的用户规则（auto-classifier.md §16.7）：快照里设置的 `autoMode` 规则，快照与会话层的 deny 规则（去重）。
#[tokio::test]
async fn the_classifier_gets_the_users_rules() {
    use rust_cli_kernel_api::permission::{RuleSource, SourcedRule, syntax::parse_rule};
    let judge = Judge::new(ClassifierVerdict::Allow);
    let mut base = rules(&[
        (RuleBehavior::Deny, "Bash(rm:*)"),
        (RuleBehavior::Allow, "Read"),
        (RuleBehavior::Deny, "Bash(rm:*)"),
    ]);
    base.auto_mode.rules.allow = vec!["Running make is fine".to_owned()];
    let gate = PolicyGate::new(base).with_classifier(judge.clone());
    let mut context = context(PermissionMode::Auto);
    context.session_rules = vec![SourcedRule {
        source: RuleSource::Session,
        behavior: RuleBehavior::Deny,
        rule: parse_rule("WebFetch(domain:example.com)"),
    }];
    let tool = Named("Bash", PermissionDecision::Passthrough);
    gate.decide(&tool, &json!({"command": "git push"}), &context)
        .await;
    assert_eq!(
        *judge.rules.lock_unpoisoned(),
        [(
            vec!["Running make is fine".to_owned()],
            vec![
                "Bash(rm:*)".to_owned(),
                "WebFetch(domain:example.com)".to_owned()
            ]
        )]
    );
}

use rust_cli_kernel_api::classifier::{NoVerdict, Unavailable, UnavailableCause};
use rust_cli_kernel_api::permission::ExecutionState;
use rust_cli_kernel_api::permission::auto_mode::DenialCounts;

fn auto() -> Arc<super::Live> {
    super::Live::new(ExecutionState::from(PermissionMode::Auto))
}

async fn run(
    gate: &PolicyGate,
    live: &Arc<super::Live>,
    tool: &'static str,
    input: Value,
) -> PermissionDecision {
    gate.decide(
        &Named(tool, PermissionDecision::Passthrough),
        &input,
        &live.context(),
    )
    .await
}

fn counts(consecutive: u32, total: u32) -> DenialCounts {
    DenialCounts { consecutive, total }
}

/// 安全名单里的工具直接放行，不交分类器：工作目录外的读在 auto 与 plan 里都放行（auto-classifier.md §16.2 第 7 步）。修前：交分类器。
#[tokio::test]
async fn the_safe_allowlist_skips_the_classifier() {
    let judge = Judge::new(ClassifierVerdict::Block {
        reason: "x".to_owned(),
    });
    let gate = PolicyGate::default().with_classifier(judge.clone());
    for live in [
        auto(),
        super::Live::new(ExecutionState::default().with_plan(true)),
    ] {
        assert_eq!(
            run(&gate, &live, "Read", json!({"file_path": "/etc/hosts"})).await,
            allow()
        );
    }
    assert!(judge.asked().is_empty());
}

/// 判的过程中换了模式：结论丢掉，退回原来的询问；换到 dontAsk 时拒绝。不计数（§16.2 第 9 步）。
#[tokio::test]
async fn a_mode_change_while_classifying_falls_back_to_asking() {
    struct Switch(Arc<super::Live>, ExecutionState);
    #[async_trait]
    impl AutoClassifier for Switch {
        async fn classify(&self, _request: ClassifyRequest<'_>) -> ClassifierVerdict {
            self.0.set(self.1);
            ClassifierVerdict::Block {
                reason: "x".to_owned(),
            }
        }
    }
    let live = auto();
    let gate = PolicyGate::default().with_classifier(Arc::new(Switch(
        live.clone(),
        ExecutionState::from(PermissionMode::Default),
    )));
    assert!(matches!(
        run(&gate, &live, "Bash", json!({"command": "git push"})).await,
        PermissionDecision::Ask { .. }
    ));
    assert_eq!(live.counts(), DenialCounts::default());
    let live = auto();
    let gate = PolicyGate::default().with_classifier(Arc::new(Switch(
        live.clone(),
        ExecutionState::from(PermissionMode::DontAsk),
    )));
    assert_eq!(
        run(&gate, &live, "Bash", json!({"command": "git push"})).await,
        deny(texts::dont_ask_rejected("Bash"))
    );
}

/// 工具开工之后、判决之前换了模式（`ctx.mode` 与会话这一刻的不同）：这次不按自动语义，原来的询问，不交分类器。
#[tokio::test]
async fn a_mode_change_before_the_decision_asks_without_classifying() {
    let judge = Judge::new(ClassifierVerdict::Allow);
    let gate = PolicyGate::default().with_classifier(judge.clone());
    let live = auto();
    let context = live.context();
    live.set(ExecutionState::from(PermissionMode::Auto).with_plan(true));
    assert!(matches!(
        gate.decide(
            &Named("Bash", PermissionDecision::Passthrough),
            &json!({"command": "git push"}),
            &context
        )
        .await,
        PermissionDecision::Ask { .. }
    ));
    assert!(judge.asked().is_empty());
}

/// 主模型不支持自动模式、或设置关掉了：照普通的判决链询问（§16.6）。
#[tokio::test]
async fn auto_needs_a_supported_model_and_the_setting() {
    let judge = Judge::new(ClassifierVerdict::Allow);
    let gate = PolicyGate::default().with_classifier(judge.clone());
    let live = auto();
    live.set_model(rust_cli_base::models::SONNET_4_5_DATED);
    assert!(matches!(
        run(&gate, &live, "Bash", json!({"command": "git push"})).await,
        PermissionDecision::Ask { .. }
    ));
    let mut disabled = rules(&[]);
    disabled.auto_mode.disabled = true;
    let gate = PolicyGate::new(disabled).with_classifier(judge.clone());
    assert!(matches!(
        run(&gate, &auto(), "Bash", json!({"command": "git push"})).await,
        PermissionDecision::Ask { .. }
    ));
    assert!(judge.asked().is_empty());
}

/// 计数（§16.3、§16.5）：拦下与解析不了各计一次，连续 3 次之后改为原来的询问；安全拦截、不可用不计；分类器放行清零连续数。
#[tokio::test]
async fn blocks_count_toward_the_limit_and_allows_reset_it() {
    let live = auto();
    let block = PolicyGate::default().with_classifier(Judge::new(ClassifierVerdict::Block {
        reason: "x".to_owned(),
    }));
    let unparseable = PolicyGate::default();
    let refused = PolicyGate::default()
        .with_classifier(Judge::new(ClassifierVerdict::NoVerdict(NoVerdict::Refused)));
    let push = || json!({"command": "git push"});
    assert!(matches!(
        run(&block, &live, "Bash", push()).await,
        PermissionDecision::Deny { .. }
    ));
    assert_eq!(
        run(&unparseable, &live, "Bash", push()).await,
        deny(texts::CLASSIFIER_UNAVAILABLE)
    );
    assert_eq!(
        run(&refused, &live, "Bash", push()).await,
        deny(texts::CLASSIFIER_REFUSED)
    );
    assert_eq!(live.counts(), counts(2, 2));
    assert!(matches!(
        run(&block, &live, "Bash", push()).await,
        PermissionDecision::Ask { .. }
    ));
    assert_eq!(live.counts(), counts(3, 3));
    let allowing = PolicyGate::default().with_classifier(Judge::new(ClassifierVerdict::Allow));
    assert_eq!(run(&allowing, &live, "Bash", push()).await, allow());
    assert_eq!(live.counts(), counts(0, 3));
}

/// 累计到 20 次：改为询问，两个数清零。
#[tokio::test]
async fn the_total_limit_asks_and_resets() {
    let live = auto();
    live.set_counts(counts(0, 19));
    let block = PolicyGate::default().with_classifier(Judge::new(ClassifierVerdict::Block {
        reason: "x".to_owned(),
    }));
    assert!(matches!(
        run(&block, &live, "Bash", json!({"command": "git push"})).await,
        PermissionDecision::Ask { .. }
    ));
    assert_eq!(live.counts(), DenialCounts::default());
}

/// 规则放行只在模式是 auto 时清零（plan 里不清，同对照实现）；快速放行在 plan 里也清。
#[tokio::test]
async fn rule_allows_reset_only_in_auto_mode() {
    let gate = PolicyGate::default();
    let live = auto();
    live.set_counts(counts(2, 2));
    assert_eq!(
        run(&gate, &live, "Bash", json!({"command": "ls"})).await,
        allow()
    );
    assert_eq!(live.counts(), counts(0, 2));
    let plan = super::Live::new(ExecutionState::default().with_plan(true));
    plan.set_counts(counts(2, 2));
    assert_eq!(
        run(&gate, &plan, "Bash", json!({"command": "ls"})).await,
        allow()
    );
    assert_eq!(plan.counts(), counts(2, 2));
    assert_eq!(
        run(&gate, &plan, "Read", json!({"file_path": "/etc/hosts"})).await,
        allow()
    );
    assert_eq!(plan.counts(), counts(0, 2));
}

/// 给模型的文字（§16.3）：拦下时按工具加“可以加一条规则”（Write 写成 Edit；安全检查不加）；不可用时写模型、原因与等待时间；对话太长问人；
/// 回合停了拒绝。
#[tokio::test]
async fn outcomes_become_the_reference_texts() {
    let block = PolicyGate::default().with_classifier(Judge::new(ClassifierVerdict::Block {
        reason: "[Git Destructive]".to_owned(),
    }));
    assert_eq!(
        run(&block, &auto(), "Write", json!({"file_path": "/tmp/a.txt"})).await,
        deny(texts::classifier_blocked("[Git Destructive]", Some("Edit")))
    );
    let rate_limited = Unavailable {
        model: rust_cli_base::models::SONNET_5.to_owned(),
        cause: UnavailableCause::RateLimited,
        retry_after_ms: None,
    };
    let unavailable = PolicyGate::default().with_classifier(Judge::new(
        ClassifierVerdict::NoVerdict(NoVerdict::Unavailable(rate_limited.clone())),
    ));
    let PermissionDecision::Deny { message } = run(
        &unavailable,
        &auto(),
        "Bash",
        json!({"command": "git push"}),
    )
    .await
    else {
        panic!("deny");
    };
    assert!(message.starts_with(&format!(
        "{} is temporarily unavailable (rate-limited), so auto mode cannot determine the safety of Bash right now.",
        rust_cli_base::models::SONNET_5
    )));
    let waiting = texts::classifier_unavailable(
        "Bash",
        &Unavailable {
            retry_after_ms: Some(30_000),
            ..rate_limited.clone()
        },
    );
    assert!(waiting.starts_with(&format!(
        "The API told auto mode's safety classifier ({}) to wait about 30 seconds from now (rate-limited), so auto mode cannot determine the safety of Bash until then.",
        rust_cli_base::models::SONNET_5
    )));
    assert!(waiting.contains("try this action again once the wait is over."));
    let long = texts::classifier_unavailable(
        "Bash",
        &Unavailable {
            retry_after_ms: Some(600_000),
            ..rate_limited
        },
    );
    assert!(long.contains("You cannot wait this out inside the turn."));
    let too_long = PolicyGate::default().with_classifier(Judge::new(ClassifierVerdict::NoVerdict(
        NoVerdict::TranscriptTooLong,
    )));
    assert!(matches!(
        run(&too_long, &auto(), "Bash", json!({"command": "git push"})).await,
        PermissionDecision::Ask { .. }
    ));
    let cancelled = PolicyGate::default().with_classifier(Judge::new(
        ClassifierVerdict::NoVerdict(NoVerdict::Cancelled),
    ));
    assert_eq!(
        run(&cancelled, &auto(), "Bash", json!({"command": "git push"})).await,
        deny(texts::CLASSIFIER_ABORTED)
    );
}

/// 危险的 allow 规则在自动语义下不生效（§16.4）：`Bash(python:*)` 不放行 `python x.py`，交分类器；classifyAllShell 时全部 Bash 规则。
/// 别的模式照常放行。
#[tokio::test]
async fn dangerous_allow_rules_do_not_apply_in_auto_mode() {
    let judge = Judge::new(ClassifierVerdict::Block {
        reason: "x".to_owned(),
    });
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Allow, "Bash(python:*)"),
        (RuleBehavior::Allow, "Bash(git status:*)"),
    ]))
    .with_classifier(judge.clone());
    let python = || json!({"command": "python x.py"});
    assert!(matches!(
        run(&gate, &auto(), "Bash", python()).await,
        PermissionDecision::Deny { .. }
    ));
    assert_eq!(
        run(&gate, &auto(), "Bash", json!({"command": "git status"})).await,
        allow()
    );
    assert_eq!(
        bash(&gate, PermissionMode::Default, "python x.py").await,
        allow()
    );
    let mut all_shell = rules(&[(RuleBehavior::Allow, "Bash(npm test:*)")]);
    all_shell.auto_mode.classify_all_shell = true;
    let gate = PolicyGate::new(all_shell).with_classifier(judge.clone());
    assert!(matches!(
        run(&gate, &auto(), "Bash", json!({"command": "npm test"})).await,
        PermissionDecision::Deny { .. }
    ));
    assert_eq!(judge.asked(), ["Bash", "Bash"]);
}
