use super::effort_level;

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// 答不出档位（测试替身、没有规则的显式覆盖）：意图原样。
#[test]
fn without_offered_levels_the_intent_is_kept() {
    assert_eq!(effort_level("max", None).as_deref(), Some("max"));
}

/// 有档位：同名的那一档用 provider 的写法；xhigh / max 没有时退到 high；仍没有时不改（None）。规则原来在 cli 的
/// `--effort`（cli-args.md §1.4），子代理的 `effort` 用同一条（model-options.md §12.2，D3）。
#[test]
fn offered_levels_decide_the_level() {
    let offered = levels(&["Low", "high"]);
    assert_eq!(effort_level("low", Some(&offered)).as_deref(), Some("Low"));
    assert_eq!(effort_level("max", Some(&offered)).as_deref(), Some("high"));
    assert_eq!(
        effort_level("XHigh", Some(&offered)).as_deref(),
        Some("high")
    );
    assert_eq!(effort_level("medium", Some(&offered)), None);
    // 档位不是这五档的模型（glm-5.1 的 disabled / enabled）：都对不上。
    let glm = levels(&["disabled", "enabled"]);
    assert_eq!(effort_level("high", Some(&glm)), None);
    assert_eq!(effort_level("max", Some(&glm)), None);
}
