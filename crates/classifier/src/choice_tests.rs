use rust_cli_base::models;
use rust_cli_kernel_api::model::{ModelSelection, ThinkingConfig};

use super::*;
use crate::test_support::{anthropic, glm};

/// 别的厂商的主模型就是分类器的模型，不探测。
#[test]
fn other_main_models_are_the_classifier_model() {
    let probes = Probes::default();
    assert_eq!(
        probes.choose(&glm()),
        Choice {
            selection: glm(),
            probing: false
        }
    );
    let kept = ModelSelection::new("anthropic", models::CLASSIFIER_KEEPS_MAIN[0]);
    assert_eq!(probes.choose(&kept).selection, kept);
}

/// Anthropic 的主模型换成同一个 provider 上的 Sonnet 5；第一次算探测，确认之后不再探测，退回之后用主模型。
#[test]
fn anthropic_main_models_probe_sonnet_five_once_per_provider() {
    let probes = Probes::default();
    let first = probes.choose(&anthropic());
    assert_eq!(
        first.selection,
        ModelSelection::new("anthropic", models::SONNET_5)
    );
    assert!(first.probing);
    probes.settle("anthropic", Probe::Confirmed);
    // 已经定下的结论不被后来的探测改掉
    probes.settle("anthropic", Probe::Demoted);
    assert!(!probes.choose(&anthropic()).probing);
    let other = ModelSelection::new("gateway", models::OPUS_5_5);
    probes.settle("gateway", Probe::Demoted);
    assert_eq!(probes.choose(&other).selection, other);
}

#[test]
fn the_one_million_suffix_carries_over() {
    let main = ModelSelection::new(
        "anthropic",
        format!("{}{}", models::OPUS_5_5, models::ONE_MILLION_SUFFIX),
    );
    assert_eq!(
        Probes::default().choose(&main).selection.model,
        format!("{}{}", models::SONNET_5, models::ONE_MILLION_SUFFIX)
    );
}

/// thinking 能关就关；不能关的模型不写 thinking，输出上限另加余量。
#[test]
fn thinking_is_disabled_where_the_model_allows_it() {
    assert_eq!(
        thinking(models::SONNET_5),
        (Some(ThinkingConfig::Disabled), 0)
    );
    assert_eq!(
        thinking(models::GLM_5_3),
        (Some(ThinkingConfig::Disabled), 0)
    );
    assert_eq!(
        thinking(models::OPUS_5_5),
        (None, ALWAYS_THINKING_EXTRA_TOKENS)
    );
}
