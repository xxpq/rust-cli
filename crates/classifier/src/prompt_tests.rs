use rust_cli_base::models;
use rust_cli_prompts::auto_mode;

use super::*;
use crate::test_support::no_rules;

#[test]
fn the_format_follows_the_severity_table() {
    assert_eq!(
        Format::for_model(models::SONNET_5),
        Format::Severity { t1: 25.0, t2: 35.0 }
    );
    assert_eq!(
        Format::for_model(&format!(
            "{}{}",
            models::OPUS_4_8,
            models::ONE_MILLION_SUFFIX
        )),
        Format::Severity { t1: 45.0, t2: 35.0 }
    );
    assert_eq!(Format::for_model(models::GLM_5_3), Format::Block);
}

/// 两种格式共用正文，只换输出说明；第一阶段有停止序列，第二阶段没有。
#[test]
fn the_formats_share_the_body() {
    let severity = Format::for_model(models::SONNET_5);
    assert_eq!(
        severity.system(no_rules()),
        format!(
            "{}{}",
            render(auto_mode::system(), no_rules()),
            auto_mode::output_severity()
        )
    );
    assert!(
        Format::Block
            .system(no_rules())
            .ends_with(auto_mode::output_block())
    );
    assert_eq!(severity.stop(Stage::One), ["</severity>"]);
    assert_eq!(Format::Block.stop(Stage::One), ["</block>"]);
    assert!(Format::Block.stop(Stage::Two).is_empty());
    assert_eq!(severity.suffix(Stage::Two), auto_mode::stage2_severity());
    assert_eq!(Format::Block.suffix(Stage::One), auto_mode::stage1_block());
}

/// 规则名取提示里的写法；替换表换过名字的允许规则不在认得的 slug 里。
#[test]
fn rule_names_come_from_the_prompt() {
    let names = RuleNames::from_prompt(&Format::Block.system(no_rules()));
    assert_eq!(names.name("data_exfiltration"), Some("Data Exfiltration"));
    assert_eq!(names.name("git_destructive"), Some("Git Destructive"));
    assert!(!known("zcode_scheduling"));
}

/// 没有用户规则时：槽的标签去掉、缺省留着，deny 规则那一行为空（同 B6，parity 测试逐字比对）。
#[test]
fn without_user_rules_the_defaults_stay() {
    let rendered = render(auto_mode::system(), no_rules());
    assert!(!rendered.contains("_to_replace>") && !rendered.contains(DENY_RULES_SLOT));
    assert!(rendered.contains("\n- Security Discussion: "));
    assert!(rendered.contains("not visible in the input.\n\n\n## ALLOW"));
}

/// 用户的规则换掉槽的缺省（auto-classifier.md §16.7）：每项一行；`$defaults` 的位置放回缺省，只放一次；没写的槽用缺省；deny
/// 规则列在 SOFT BLOCK 的后面。
#[test]
fn user_rules_replace_the_slots() {
    let rules = AutoModeRules {
        allow: vec!["Running make is fine".to_owned()],
        soft_deny: vec![
            "$defaults".to_owned(),
            "Never touch prod".to_owned(),
            "$defaults".to_owned(),
        ],
        ..AutoModeRules::default()
    };
    let deny = ["Bash(rm:*)".to_owned(), "Edit(/etc/**)".to_owned()];
    let rendered = render(
        auto_mode::system(),
        UserRules {
            auto_mode: &rules,
            deny: &deny,
        },
    );
    assert!(rendered.contains("## ALLOW"));
    assert!(!rendered.contains("- Security Discussion: "));
    assert!(rendered.contains("\n- Running make is fine\n\n</automode_permissions>"));
    assert_eq!(rendered.matches("- Git Destructive [").count(), 1);
    assert!(rendered.contains(
        "not visible in the input.\n- Never touch prod\n- User Deny Rules: The user has configured these permission deny rules: `Bash(rm:*)`, `Edit(/etc/**)`. Each rule names a tool"
    ));
    assert!(rendered.contains("\n- Data Exfiltration: "));
}
