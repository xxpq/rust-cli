use super::*;

#[test]
fn the_default_turns_nothing_off() {
    let preferences = SessionPreferences::default();
    assert!(preferences.auto_memory && preferences.tool_search);
    assert!(preferences.is_default());
    assert_eq!(
        preferences.disallowed(&["CronCreate".to_owned()]),
        ["CronCreate"]
    );
}

#[test]
fn tool_search_off_adds_it_to_the_run_list_once() {
    let off = SessionPreferences {
        tool_search: false,
        ..SessionPreferences::default()
    };
    assert_eq!(
        off.disallowed(&["CronCreate".to_owned()]),
        ["CronCreate", "ToolSearch"]
    );
    // 派生的子代理从父运行的名单继承到它，偏好又关一次：只留一份
    assert_eq!(off.disallowed(&["ToolSearch".to_owned()]), ["ToolSearch"]);
}

#[test]
fn a_missing_field_reads_as_not_turned_off() {
    let read: SessionPreferences = serde_json::from_str(r#"{"auto_memory": false}"#).unwrap();
    assert_eq!(
        read,
        SessionPreferences {
            auto_memory: false,
            tool_search: true,
        }
    );
}
