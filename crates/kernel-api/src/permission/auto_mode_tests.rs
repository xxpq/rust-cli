use super::*;

fn state(mode: PermissionMode, plan: bool) -> ExecutionState {
    ExecutionState::from(mode).with_plan(plan)
}

/// auto 生不生效（auto-classifier.md §16.6）：plan 从 default / acceptEdits / dontAsk / auto 进入看设置，从 yolo / guarded 进入不生效。
#[test]
fn auto_is_active_in_auto_mode_and_in_plan_unless_entered_from_bypass() {
    let on = AutoModeSettings::default();
    let glm = Some(models::GLM_5_3);
    assert!(auto_active(state(PermissionMode::Auto, false), &on, glm));
    for mode in [
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
    ] {
        assert!(auto_active(state(mode, true), &on, glm), "{mode:?}");
        assert!(
            !auto_active(state(mode, false), &on, glm) || mode == PermissionMode::Auto,
            "{mode:?}"
        );
    }
    for mode in [PermissionMode::BypassPermissions, PermissionMode::Guarded] {
        assert!(!auto_active(state(mode, true), &on, glm), "{mode:?}");
    }
    let off_in_plan = AutoModeSettings {
        use_auto_mode_during_plan: false,
        ..AutoModeSettings::default()
    };
    assert!(!auto_active(
        state(PermissionMode::Default, true),
        &off_in_plan,
        glm
    ));
    assert!(auto_active(
        state(PermissionMode::Auto, false),
        &off_in_plan,
        glm
    ));
    let disabled = AutoModeSettings {
        disabled: true,
        ..AutoModeSettings::default()
    };
    assert!(!auto_active(
        state(PermissionMode::Auto, false),
        &disabled,
        glm
    ));
    // 主模型不支持时不生效；没有主模型时算支持。
    assert!(!auto_active(
        state(PermissionMode::Auto, false),
        &on,
        Some(models::SONNET_4_5_DATED)
    ));
    assert!(auto_active(state(PermissionMode::Auto, false), &on, None));
}

#[test]
fn old_anthropic_models_and_third_party_exclusions_do_not_support_auto_mode() {
    let unsupported = models::AUTO_MODE_TOO_OLD
        .iter()
        .chain(&models::AUTO_MODE_THIRD_PARTY_UNSUPPORTED)
        .map(|model| (*model).to_owned())
        .chain([
            models::HAIKU_3_5_DATED.to_owned(),
            models::SONNET_4_5_DATED.to_owned(),
            models::OPUS_4_1.to_ascii_uppercase(),
            format!(
                "{}{}",
                models::AUTO_MODE_THIRD_PARTY_UNSUPPORTED[1],
                models::ONE_MILLION_SUFFIX
            ),
        ]);
    for model in unsupported {
        assert!(!supports_auto_mode(&model), "{model}");
    }
    for model in [
        models::OPUS_5_5.to_owned(),
        models::SONNET_5.to_owned(),
        format!("{}{}", models::OPUS_4_8, models::ONE_MILLION_SUFFIX),
        models::HAIKU_5_5.to_owned(),
        models::GLM_5_3.to_owned(),
        models::DEEPSEEK_V4_PRO.to_owned(),
    ] {
        assert!(supports_auto_mode(&model), "{model}");
    }
}

/// 计数（auto-classifier.md §16.5）：连续 3 次回 Consecutive 且不清零；累计 20 次回 Total、两个数清零；放行只清连续。
#[test]
fn denials_count_up_to_the_limits() {
    let mut counts = DenialCounts::default();
    let mut limits = Vec::new();
    for _ in 0..4 {
        let (next, limit) = counts.record(AutoVerdict::Blocked);
        counts = next;
        limits.push(limit);
    }
    assert_eq!(
        limits,
        [
            None,
            None,
            Some(DenialLimit::Consecutive(3)),
            Some(DenialLimit::Consecutive(4))
        ]
    );
    let (counts, limit) = counts.record(AutoVerdict::Allowed);
    assert_eq!(
        (counts, limit),
        (
            DenialCounts {
                consecutive: 0,
                total: 4
            },
            None
        )
    );
    let near = DenialCounts {
        consecutive: 0,
        total: MAX_TOTAL_DENIALS - 1,
    };
    assert_eq!(
        near.record(AutoVerdict::Blocked),
        (
            DenialCounts::default(),
            Some(DenialLimit::Total(MAX_TOTAL_DENIALS))
        )
    );
}

/// 规则快照里的设置：旧的序列化结果没有这一项时是缺省（plan 开自动语义）。
#[test]
fn settings_default_when_absent() {
    let parsed: AutoModeSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(parsed, AutoModeSettings::default());
    assert!(parsed.use_auto_mode_during_plan);
}
