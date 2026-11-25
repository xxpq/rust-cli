use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::models;
use rust_cli_kernel_api::classifier::{ClassifierVerdict, NoVerdict};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::ids::{RunId, SessionId};
use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::model::{ModelError, ModelSelection, QuerySource, ThinkingConfig};
use rust_cli_kernel_api::side_request::{Requester, SideSource};
use rust_cli_kernel_api::testing::{FixedClock, ScriptedModel, TokioClock};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::prompt::{Format, RuleNames};
use crate::test_support::{no_rules, reply};

const HISTORY: &str =
    "{\"user\":\"Run the scripted shell checks.\"}\n{\"Bash\":\"echo planning\"}\n";
const ACTION: &str = "{\"Bash\":\"touch plan.txt\"}\n";

/// 计时跟着 tokio 的时钟走（暂停时间的测试里自动前进）。
fn job<'a>(format: Format, names: &'a RuleNames, clock: &'a dyn Clock) -> Job<'a> {
    Job {
        format,
        system: format.system(no_rules()),
        names,
        history: vec![HISTORY.to_owned()],
        action: ACTION.to_owned(),
        requester: Requester {
            session: SessionId::new("s1"),
            run: Some(RunId::new("r1")),
            agent: None,
            root: None,
        },
        cancel: CancellationToken::new(),
        attempt: Duration::from_secs(60),
        clock,
    }
}

fn sonnet() -> ModelSelection {
    ModelSelection::new("anthropic", models::SONNET_5)
}

/// 第一阶段的请求形状同 B6（auto-classifier.md §15.6.3）：一块提示带缓存标记，user 是摘录的五块，64、`</severity>`、
/// thinking 关、请求级重试 4 次。
#[test]
fn the_first_stage_request_has_the_b6_shape() {
    let clock = FixedClock(0);
    let format = Format::for_model(models::SONNET_5);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let request = job(format, &names, &clock).request(&sonnet(), Stage::One);
    assert_eq!(request.source, QuerySource::Side(SideSource::AutoMode));
    assert_eq!(request.max_tokens, 64);
    assert_eq!(request.stop_sequences, ["</severity>"]);
    assert_eq!(request.thinking, Some(ThinkingConfig::Disabled));
    assert_eq!(request.temperature, None);
    assert!(request.tools.is_empty());
    assert_eq!(request.options.max_retries, Some(4));
    assert_eq!(request.system.len(), 1);
    assert_eq!(
        serde_json::to_value(&request.messages).unwrap(),
        json!([{"role": "user", "content": [
            {"type": "text", "text": "<transcript>\n"},
            {"type": "text", "text": HISTORY, "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": ACTION, "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": "</transcript>\n"},
            {"type": "text", "text": rust_cli_prompts::auto_mode::stage1_severity()},
        ]}])
    );
    let two = job(format, &names, &clock).request(&sonnet(), Stage::Two);
    assert_eq!(two.max_tokens, 8_192);
    assert!(two.stop_sequences.is_empty());
    // 平台层的 maxOutputTokens 补丁以请求自己的上限为输入（同 WebSearch 的旁路请求，auto-classifier.md §16.16）。修前不给，补丁按
    // 配置的上限把 64 改成 128000。
    assert_eq!(request.options.provider_max_output_tokens, Some(64));
    assert_eq!(two.options.provider_max_output_tokens, Some(8_192));
}

/// B6：回复都解析不了，两个阶段各发 5 次，结论是“解析不了”。
#[tokio::test]
async fn unparseable_replies_are_resent_four_times_per_stage() {
    let clock = TokioClock::new(0);
    let format = Format::for_model(models::SONNET_5);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let model = ScriptedModel::new(
        (0..10)
            .map(|_| reply("I need more context.", StopReason::EndTurn))
            .collect(),
    );
    let judged = job(format, &names, &clock).judge(&model, &sonnet()).await;
    assert_eq!(
        judged,
        Judged::Verdict(ClassifierVerdict::NoVerdict(NoVerdict::Unparseable))
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 10);
    assert!(requests[..5].iter().all(|request| request.max_tokens == 64));
    assert!(
        requests[5..]
            .iter()
            .all(|request| request.max_tokens == 8_192)
    );
}

/// B6b：第一阶段回 `<severity>10`，只发一次就放行。
#[tokio::test]
async fn a_low_first_stage_score_allows_at_once() {
    let clock = TokioClock::new(0);
    let format = Format::for_model(models::SONNET_5);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let model = ScriptedModel::new(vec![reply("<severity>10", StopReason::StopSequence)]);
    let judged = job(format, &names, &clock).judge(&model, &sonnet()).await;
    assert_eq!(judged, Judged::Verdict(ClassifierVerdict::Allow));
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn the_second_stage_blocks_with_the_rule_name() {
    let clock = TokioClock::new(0);
    let format = Format::for_model(models::SONNET_5);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let model = ScriptedModel::new(vec![
        reply("<severity>60", StopReason::StopSequence),
        reply(
            "<thinking>sends secrets</thinking><severity>80</severity><category>Data Exfiltration</category>",
            StopReason::EndTurn,
        ),
    ]);
    assert_eq!(
        job(format, &names, &clock).judge(&model, &sonnet()).await,
        Judged::Verdict(ClassifierVerdict::Block {
            reason: "[Data Exfiltration]".to_owned()
        })
    );
}

/// `<block>` 格式（GLM 等）：第一阶段拦下、第二阶段放行。
#[tokio::test]
async fn the_block_format_runs_both_stages() {
    let clock = TokioClock::new(0);
    let names = RuleNames::from_prompt(&Format::Block.system(no_rules()));
    let model = ScriptedModel::new(vec![
        reply(
            "<block>yes</block><reason>[Git Destructive] r</reason>",
            StopReason::StopSequence,
        ),
        reply(
            "<thinking>fine</thinking><block>no</block>",
            StopReason::EndTurn,
        ),
    ]);
    let glm = ModelSelection::new("zhipu", models::GLM_5_3);
    let job = job(Format::Block, &names, &clock);
    assert_eq!(
        job.judge(&model, &glm).await,
        Judged::Verdict(ClassifierVerdict::Allow)
    );
    let requests = model.requests();
    assert_eq!(requests[0].stop_sequences, ["</block>"]);
    assert_eq!(requests[0].selection, glm);
}

/// 第一阶段判拦下、第二阶段拒绝回答：照第一阶段拦下；拒绝不重发。
#[tokio::test]
async fn a_second_stage_refusal_keeps_the_first_stage_block() {
    let clock = TokioClock::new(0);
    let names = RuleNames::from_prompt(&Format::Block.system(no_rules()));
    let model = ScriptedModel::new(vec![
        reply(
            "<block>yes</block><reason>[Git Destructive] r</reason>",
            StopReason::StopSequence,
        ),
        reply("", StopReason::Refusal),
    ]);
    assert_eq!(
        job(Format::Block, &names, &clock)
            .judge(&model, &ModelSelection::new("zhipu", models::GLM_5_3))
            .await,
        Judged::Verdict(ClassifierVerdict::Block {
            reason: "[Git Destructive] r".to_owned()
        })
    );
    assert_eq!(model.requests().len(), 2);
}

/// 第二阶段的请求失败：照第一阶段的回复拦下（理由写明是第二阶段出错）；第一阶段的请求失败交给调用方。
#[tokio::test]
async fn request_failures_by_stage() {
    let clock = TokioClock::new(0);
    let format = Format::for_model(models::SONNET_5);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let server = || ModelError::Server {
        status: 500,
        message: "boom".to_owned(),
    };
    let second = ScriptedModel::new(vec![
        reply("<severity>60", StopReason::StopSequence),
        vec![Err(server())],
    ]);
    assert_eq!(
        job(format, &names, &clock).judge(&second, &sonnet()).await,
        Judged::Verdict(ClassifierVerdict::Block {
            reason: crate::texts::STAGE_TWO_ERROR.to_owned()
        })
    );
    let first = ScriptedModel::new(vec![vec![Err(server())]]);
    assert_eq!(
        job(format, &names, &clock).judge(&first, &sonnet()).await,
        Judged::FirstStageFailed(Failed::Model(server()))
    );
}

/// 请求时限：模型一直不回，到了时限这次请求失败（不改任何状态）。
#[tokio::test(start_paused = true)]
async fn a_silent_model_times_out() {
    let clock = TokioClock::new(0);
    let format = Format::for_model(models::SONNET_5);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let model = ScriptedModel::new(vec![reply("<severity>10", StopReason::StopSequence)])
        .gated(Arc::new(Notify::new()));
    assert_eq!(
        job(format, &names, &clock).judge(&model, &sonnet()).await,
        Judged::FirstStageFailed(Failed::TimedOut)
    );
}

/// 第二阶段的回复文字为空（不是 refusal、不是输出上限）：不重发，结论是解析不了，不是安全拦截（auto-classifier.md §16.3）。
/// 修前是 `Refused`：给模型的是拦截的那一句，也不计数；对照实现只把 `stop_reason` 是 `refusal` 的记成安全拦截。
#[tokio::test]
async fn an_empty_second_stage_reply_is_unparseable() {
    let clock = TokioClock::new(0);
    let format = Format::for_model(models::SONNET_5);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let model = ScriptedModel::new(vec![
        reply("<severity>60", StopReason::StopSequence),
        reply("", StopReason::EndTurn),
    ]);
    assert_eq!(
        job(format, &names, &clock).judge(&model, &sonnet()).await,
        Judged::Verdict(ClassifierVerdict::NoVerdict(NoVerdict::Unparseable))
    );
    assert_eq!(model.requests().len(), 2);
    // 两个阶段都是 refusal：安全拦截。
    let refused = ScriptedModel::new(vec![
        reply("", StopReason::Refusal),
        reply("", StopReason::Refusal),
    ]);
    assert_eq!(
        job(format, &names, &clock).judge(&refused, &sonnet()).await,
        Judged::Verdict(ClassifierVerdict::NoVerdict(NoVerdict::Refused))
    );
}

/// MODEL-OPTIONS 合入时（model-options.md §12.4，auto-classifier.md §13.1 第 2 项）：分类器的请求要关思考，档位照 provider 的配置
/// ——有「关」这一档（`disabled` 等）时用它，补丁写 provider 的「关」；没有时取第一档（最接近关）；答不出档位时照会话的选择。
/// 以前只写 Anthropic 的 `{type: "disabled"}`，平台层的补丁按会话的档位（没有时最后一档）把 thinking 改回开着。
#[tokio::test]
async fn the_request_takes_the_providers_off_level() {
    let clock = TokioClock::new(0);
    let format = Format::for_model(models::GLM_5_3);
    let names = RuleNames::from_prompt(&format.system(no_rules()));
    let levels = |names: &[&str]| Some(names.iter().map(|name| (*name).to_owned()).collect());
    let glm = ModelSelection {
        reasoning: Some("enabled".to_owned()),
        ..crate::test_support::glm()
    };
    for (offered, expected) in [
        (levels(&["disabled", "enabled"]), Some("disabled")),
        (levels(&["low", "high", "max"]), Some("low")),
        (None, Some("enabled")),
    ] {
        let model =
            ScriptedModel::new(vec![reply("<block>no", StopReason::StopSequence)]).levels(offered);
        job(format, &names, &clock).judge(&model, &glm).await;
        assert_eq!(model.requests()[0].selection.reasoning.as_deref(), expected);
    }
}
