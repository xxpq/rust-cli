use std::collections::BTreeMap;

use serde_json::json;

use super::*;

fn context(declares: bool) -> PluginContext {
    PluginContext {
        id: "demo@inline".to_owned(),
        name: "demo".to_owned(),
        root: "/p/demo".to_owned(),
        data: Some("/s/data/demo@inline".to_owned()),
        options: BTreeMap::from([
            ("color".to_owned(), PluginOption::Plain("blue".to_owned())),
            (
                "api-token".to_owned(),
                PluginOption::Secret(RedactedString::new("s3cret")),
            ),
        ]),
        declares_options: declares,
    }
}

#[test]
fn scalars_follow_js_string() {
    assert_eq!(PluginOption::scalar(&json!("a")).as_deref(), Some("a"));
    assert_eq!(PluginOption::scalar(&json!(true)).as_deref(), Some("true"));
    assert_eq!(PluginOption::scalar(&json!(3)).as_deref(), Some("3"));
    assert_eq!(PluginOption::scalar(&json!(1.5)).as_deref(), Some("1.5"));
    assert_eq!(PluginOption::scalar(&json!(null)), None);
    assert_eq!(PluginOption::scalar(&json!([1])), None);
}

#[test]
fn paths_take_both_names_and_leave_data_without_a_root() {
    let context = context(false);
    assert_eq!(
        context
            .expand_paths("${CLAUDE_PLUGIN_ROOT}/a ${ZCODE_PLUGIN_DATA}/b ${CLAUDE_PLUGIN_DATA}"),
        "/p/demo/a /s/data/demo@inline/b /s/data/demo@inline"
    );
    let without = PluginContext {
        data: None,
        ..context
    };
    assert_eq!(
        without.expand_paths("${ZCODE_PLUGIN_ROOT} ${ZCODE_PLUGIN_DATA}"),
        "/p/demo ${ZCODE_PLUGIN_DATA}"
    );
}

#[test]
fn content_hides_sensitive_options_and_keeps_missing_ones() {
    let placeholder = |key: &str| format!("[hidden {key}]");
    let same = |value: &str| value.to_owned();
    let text =
        "${user_config.color} ${user_config.api-token} ${user_config.nope} ${CLAUDE_PLUGIN_ROOT}";
    assert_eq!(
        context(true).expand_content(text, &placeholder, &same),
        "blue [hidden api-token] ${user_config.nope} /p/demo"
    );
    // 没声明 userConfig 时只换目录变量。
    assert_eq!(
        context(false).expand_content(text, &placeholder, &same),
        "${user_config.color} ${user_config.api-token} ${user_config.nope} /p/demo"
    );
    let shout = |value: &str| value.to_uppercase();
    assert_eq!(
        context(true).expand_content("${user_config.color}", &placeholder, &shout),
        "BLUE"
    );
}

#[test]
fn plain_fields_reject_sensitive_and_missing_options() {
    let options = context(true).options;
    assert_eq!(
        expand_plain_options("--color=${user_config.color}", &options),
        Ok("--color=blue".to_owned())
    );
    assert_eq!(
        expand_plain_options("${user_config.api-token}", &options),
        Err(OptionError::Sensitive("api-token".to_owned()))
    );
    assert_eq!(
        expand_plain_options("${user_config.nope}", &options),
        Err(OptionError::Missing("nope".to_owned()))
    );
    assert_eq!(
        expand_any_options("Bearer ${user_config.api-token}", &options),
        Ok("Bearer s3cret".to_owned())
    );
    // 没有结束的 `}`：原样留下。
    assert_eq!(
        expand_plain_options("${user_config.color", &options),
        Ok("${user_config.color".to_owned())
    );
}

#[test]
fn hook_env_has_paths_identity_and_options_in_both_names() {
    let env: BTreeMap<String, String> = context(true).hook_env().into_iter().collect();
    assert_eq!(env["ZCODE_PLUGIN_ROOT"], "/p/demo");
    assert_eq!(env["CLAUDE_PLUGIN_DATA"], "/s/data/demo@inline");
    assert_eq!(env["ZCODE_PLUGIN_ID"], "demo@inline");
    assert_eq!(env["ZCODE_PLUGIN_NAME"], "demo");
    assert_eq!(env["ZCODE_PLUGIN_OPTION_API_TOKEN"], "s3cret");
    assert_eq!(
        env[&format!("{}_COLOR", vars::CLAUDE_PLUGIN_OPTION)],
        "blue"
    );
    assert!(mentions_options("echo ${user_config.color}"));
    assert!(!mentions_options("echo $ZCODE_PLUGIN_OPTION_COLOR"));
}

#[test]
fn ids_are_sanitized_like_node() {
    assert_eq!(sanitize_id("demo@my.market"), "demo@my.market");
    assert_eq!(sanitize_id("a b/c\\d:e"), "a-b-c-d-e");
}

#[test]
fn secrets_never_print() {
    let option = PluginOption::Secret(RedactedString::new("s3cret"));
    assert!(!format!("{option:?}").contains("s3cret"));
    assert!(option.is_secret());
}
