//! 每 token 字符数（context-usage.md §3，修复线 F10）。

use rust_cli_base::models::{DEEPSEEK_V4_PRO, GLM_5_1, GLM_5_3, GLM_5_3_ACCOUNT, OPUS_5_5};

use super::{
    behavior, card, card_effort, card_level, card_output_limit, chars_per_token,
    mid_conversation_system, quiet_reasoning, side_reasoning,
};

/// 要关思考的旁路请求（分类器，model-options.md §12.4）：provider 有「关」这一档时用它，没有时第一档，答不出时照会话的选择。
#[test]
fn quiet_requests_take_the_off_level_or_the_first() {
    assert_eq!(
        quiet_reasoning(Some("enabled"), Some(&levels(&["disabled", "enabled"]))).as_deref(),
        Some("disabled")
    );
    assert_eq!(
        quiet_reasoning(Some("max"), Some(&levels(&["None", "low", "high"]))).as_deref(),
        Some("None")
    );
    assert_eq!(
        quiet_reasoning(Some("max"), Some(&levels(&["low", "high", "max"]))).as_deref(),
        Some("low")
    );
    assert_eq!(quiet_reasoning(Some("max"), None).as_deref(), Some("max"));
}

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// MO-2（model-options.md §4.4、§12.2，D5）：旁路请求在配置路（没有自己的卡片）上取 provider 的第一档（Node
/// `auxiliaryModelOptions`）；卡片路照会话的选择；答不出档位时照会话的选择。
#[test]
fn side_requests_take_the_first_level_on_the_config_path() {
    let glm = levels(&["low", "high", "max"]);
    assert_eq!(
        side_reasoning(GLM_5_3, Some("max"), Some(&glm)).as_deref(),
        Some("low")
    );
    assert_eq!(
        side_reasoning(GLM_5_3, Some("max"), None).as_deref(),
        Some("max")
    );
    let five = levels(&["low", "medium", "high", "xhigh", "max"]);
    assert_eq!(
        side_reasoning(OPUS_5_5, Some("max"), Some(&five)).as_deref(),
        Some("max")
    );
    assert_eq!(side_reasoning(OPUS_5_5, None, Some(&five)), None);
}

/// MO-2（model-options.md 待确认 6，D4）：卡片模型的选择没有档位时，provider 有卡片的缺省档位就用它（补丁按它求值，与卡片写的
/// effort 相同）；provider 没有它、答不出或模型没有卡片时没有。
#[test]
fn a_card_model_without_a_level_takes_the_card_default() {
    let five = levels(&["low", "medium", "high", "xhigh", "max"]);
    assert_eq!(card_level(OPUS_5_5, Some(&five)).as_deref(), Some("medium"));
    assert_eq!(card_level(OPUS_5_5, Some(&levels(&["low", "high"]))), None);
    assert_eq!(card_level(OPUS_5_5, None), None);
    assert_eq!(card_level(GLM_5_3, Some(&levels(&["low", "medium"]))), None);
}

#[test]
fn current_and_unknown_models_use_three_chars_per_token() {
    for model in [
        "claude-opus-5-5",
        "claude-opus-5-5[1m]",
        "glm-4.6",
        "gpt-5",
        "claude-opus-4-8",
    ] {
        assert_eq!(chars_per_token(model), 3, "{model}");
    }
}

#[test]
fn the_fourteen_legacy_models_use_four() {
    for model in [
        "claude-3-opus",
        "claude-3-5-sonnet-20241022",
        "claude-3.7-sonnet",
        "claude-opus-4-1",
        "claude-opus-4-6[1m]",
        "claude-sonnet-4-5-20250929",
        "claude_haiku_4_5",
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "claude-opus-4-5@20251101",
    ] {
        assert_eq!(chars_per_token(model), 4, "{model}");
    }
}

#[test]
fn a_missing_model_uses_four_and_lookalikes_do_not_match() {
    assert_eq!(chars_per_token(""), 4);
    // 只在旧模型 ID 的边界上认：更长的版本号不是旧模型。
    assert_eq!(chars_per_token("claude-opus-4-10"), 3);
    assert_eq!(chars_per_token("xclaude-3-opus"), 3);
}

/// 修复线 F23：effort 只随卡片写；没有卡片的模型即使选了档位也不写（推理字段由 option map 写，同 Node）。
/// MO-1（model-options.md §4.2）：只看模型自己的卡片（卡片路），GLM 不再借 opus 的——F121 时 GLM 写卡片认识的档位或缺省
/// `medium`，选 `enabled` 也是 `medium`。
#[test]
fn only_models_with_their_own_card_get_an_effort() {
    assert_eq!(card_effort(OPUS_5_5, None).as_deref(), Some("medium"));
    assert_eq!(card_effort(OPUS_5_5, Some("high")).as_deref(), Some("high"));
    assert_eq!(card_effort(GLM_5_3, None), None);
    assert_eq!(card_effort(GLM_5_3, Some("high")), None);
    assert_eq!(card_effort(GLM_5_1, Some("enabled")), None);
    assert_eq!(card_effort(DEEPSEEK_V4_PRO, Some("max")), None);
}

/// 对话中间的 system：生效配置给出时只看它；答不出时按卡片的内核行为回退（GLM 借 opus 的，F121，MO-1 保留）。
#[test]
fn the_config_decides_mid_conversation_system() {
    assert!(mid_conversation_system(GLM_5_1, Some(true)));
    assert!(!mid_conversation_system(OPUS_5_5, Some(false)));
    assert!(mid_conversation_system(OPUS_5_5, None));
    assert!(mid_conversation_system(GLM_5_1, None));
    assert!(!mid_conversation_system(DEEPSEEK_V4_PRO, None));
}

/// 工作线 F121 与 MO-1（model.md §11.1，model-options.md §4.3）：GLM 只借 opus 那张卡片的内核行为；请求参数、身份与上限
/// 都不借——没有自己的卡片（请求参数走配置路），输出上限只按目录（`configured`）；有卡片的模型的上限是卡片的值；没有内核
/// 行为的模型两样都没有。
#[test]
fn glm_borrows_only_the_kernel_behavior() {
    let opus = behavior(OPUS_5_5).unwrap();
    assert_eq!(behavior(GLM_5_3), Some(opus));
    assert_eq!(behavior(GLM_5_3_ACCOUNT), Some(opus));
    assert!(opus.system_turns && opus.silent_turn_reminder);
    assert_eq!(behavior(DEEPSEEK_V4_PRO), None);
    let card_path = card(OPUS_5_5).unwrap();
    assert!(card_path.adaptive_thinking && card_path.per_turn_effort);
    assert_eq!(card_path.default_effort.as_deref(), Some("medium"));
    assert!(card(GLM_5_3).is_none());
    assert_eq!(card_output_limit(OPUS_5_5, Some(16_384)), Some(128_000));
    assert_eq!(card_output_limit(GLM_5_3, Some(131_072)), Some(131_072));
    assert_eq!(card_output_limit(GLM_5_3, None), None);
    // MO-3（model-options.md 待确认 9）：没有卡片的模型都按配置的上限（以前只有 GLM，别的是 None：调用方用 32000）。
    assert_eq!(
        card_output_limit(DEEPSEEK_V4_PRO, Some(384_000)),
        Some(384_000)
    );
    assert_eq!(card_output_limit(DEEPSEEK_V4_PRO, None), None);
}

/// GLM 与 DeepSeek 不在旧模型清单里：每 token 3 个字符。
#[test]
fn glm_and_deepseek_use_three_chars_per_token() {
    for model in [GLM_5_3, GLM_5_3_ACCOUNT, DEEPSEEK_V4_PRO] {
        assert_eq!(chars_per_token(model), 3, "{model}");
    }
}

/// 没有卡片的模型的缺省输出上限只有一个（F58）。
#[test]
fn the_unknown_model_output_limit_is_32000() {
    assert_eq!(super::UNKNOWN_MAX_OUTPUT_TOKENS, 32_000);
}
