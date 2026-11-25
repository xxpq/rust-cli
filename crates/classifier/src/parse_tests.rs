use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::side_request::SideReply;

use super::*;
use crate::prompt::{Format, RuleNames};
use crate::test_support::{no_rules, reply};

#[test]
fn severity_needs_one_score_after_thinking_and_a_normal_stop() {
    let stop = Some(StopReason::StopSequence);
    assert_eq!(severity("<severity>10", stop), Some(10.0));
    assert_eq!(
        severity("<severity> 72.5 </severity>", Some(StopReason::EndTurn)),
        Some(72.5)
    );
    assert_eq!(severity("<severity>10", Some(StopReason::MaxTokens)), None);
    assert_eq!(severity("<severity>10</severity><severity>20", stop), None);
    assert_eq!(
        severity(
            "<thinking>maybe <severity>90</severity></thinking><severity>20",
            stop
        ),
        Some(20.0)
    );
    assert_eq!(severity("<thinking>still going <severity>20", stop), None);
    assert_eq!(severity("I cannot help with that.", stop), None);
}

#[test]
fn block_needs_one_value() {
    assert_eq!(block("<block>no</block>"), Some(false));
    assert_eq!(block("<BLOCK>Yes</block><reason>r</reason>"), Some(true));
    // 全文里出现两种值（含 thinking 里的）时解析不了
    assert_eq!(
        block("<thinking><block>yes</block></thinking><block>no</block>"),
        None
    );
    // 只在 thinking 里有
    assert_eq!(block("<thinking><block>no</block>"), None);
    assert_eq!(block("<block>nope</block>"), None);
}

#[test]
fn reasons_and_categories() {
    assert_eq!(
        reason(
            "<thinking><reason>x</reason></thinking><reason>  [Git Destructive] force push </reason>"
        ),
        Some("[Git Destructive] force push".to_owned())
    );
    let names = RuleNames::from_prompt(&Format::Block.system(no_rules()));
    assert_eq!(
        category(
            "<severity>80</severity><category>Data Exfiltration</category>",
            &names
        ),
        Some("[Data Exfiltration]".to_owned())
    );
    // 写法不同也按 slug 认，理由用提示里的规则名
    assert_eq!(
        category("<category>data-exfiltration</category>", &names),
        Some("[Data Exfiltration]".to_owned())
    );
    assert_eq!(category("<category>Made Up Rule</category>", &names), None);
    assert_eq!(
        category(
            "<category>Data Exfiltration</category><category>Git Destructive</category>",
            &names
        ),
        None
    );
    assert_eq!(
        slug("  Irreversible Deletion (general) "),
        "irreversible_deletion_general"
    );
}

fn collected(text: &str, stop: StopReason) -> SideReply {
    let mut out = SideReply::default();
    for event in reply(text, stop) {
        out.push(event.unwrap());
    }
    out
}

/// 拒绝：结束原因是拒绝，或者没有文字且不是因为输出上限。
#[test]
fn refusals_are_not_resent() {
    assert!(refused(&collected("", StopReason::EndTurn)));
    assert!(refused(&collected("no", StopReason::Refusal)));
    assert!(!refused(&collected("", StopReason::MaxTokens)));
    assert!(!refused(&collected("hmm", StopReason::EndTurn)));
}
