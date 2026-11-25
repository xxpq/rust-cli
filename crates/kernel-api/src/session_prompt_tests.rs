use super::*;

/// 落库的写法读得回来；只记下最后写的那一份（键固定，存储按键合并）。
#[test]
fn a_pin_reads_back_from_its_entry() {
    let pin = SystemPin {
        custom: None,
        append: Some("Always end with DONE.".to_owned()),
        shell_escape: true,
    };
    let Fact::Entry { kind, id, data } = pin.fact() else {
        panic!("a pin is an entry");
    };
    assert_eq!(kind, entry_kinds::SYSTEM_PIN);
    assert_eq!(
        data,
        serde_json::json!({"append": "Always end with DONE.", "shell_escape": true})
    );
    let entries = vec![(id, data)];
    assert_eq!(SystemPin::recorded(&entries), Some(pin));
}

/// 这个改动之前建的会话没有记录：调用方用内核现在的输入。
#[test]
fn an_old_session_has_no_pin() {
    assert_eq!(SystemPin::recorded(&[]), None);
}

/// 内核的缺省：`shell_escape` 跟着前端能力；换 system 那一层时附件那一层不变。
#[test]
fn the_default_pins_the_shell_hint_of_the_frontend() {
    let traits = FrontendTraits {
        shell_escape: true,
        typed_mentions: true,
    };
    let prompt = SessionPrompt::new(None, Some("A".to_owned()), traits.clone());
    assert!(prompt.system.shell_escape);
    let resumed = prompt.with_system(SystemPin::default());
    assert_eq!(resumed.system, SystemPin::default());
    assert_eq!(resumed.live.traits, traits);
}
