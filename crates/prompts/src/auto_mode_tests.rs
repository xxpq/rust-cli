use super::*;

/// 数据文件都在、形状对：提示以安全监视器开头、停在输出说明的标题；两种输出说明各是一节；后缀以换行开头。
#[test]
fn the_classifier_prompt_files_load() {
    assert!(system().starts_with("You are a security monitor for autonomous AI coding agents."));
    assert!(system().ends_with("## Output Format\n\n"));
    assert!(output_severity().starts_with("Output <severity>N</severity>"));
    assert!(output_block().starts_with("If the action should be blocked:"));
    for suffix in [
        stage1_severity(),
        stage2_severity(),
        stage1_block(),
        stage2_block(),
    ] {
        assert!(suffix.starts_with('\n'), "{suffix:?}");
    }
    assert!(stage1_severity().ends_with("No other text.\n"));
    assert!(compaction_summary().starts_with("[Compaction summary"));
    assert_eq!(categories().len(), 79);
    assert!(categories().iter().any(|slug| slug == "data_exfiltration"));
    assert!(deny_rules().starts_with("- User Deny Rules: ") && deny_rules().contains("{{rules}}"));
    // 用户规则的四个槽与 deny 规则的一行各一个。
    for tag in [
        "user_environment_to_replace",
        "user_hard_deny_rules_to_replace",
        "user_soft_deny_rules_to_replace",
        "user_allow_rules_to_replace",
    ] {
        assert_eq!(system().matches(&format!("<{tag}>")).count(), 1, "{tag}");
        assert_eq!(system().matches(&format!("</{tag}>")).count(), 1, "{tag}");
    }
    assert_eq!(system().matches("\n<settings_deny_rules>\n").count(), 1);
}
