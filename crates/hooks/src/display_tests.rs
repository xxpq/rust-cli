//! 运行事实里的文字（hooks.md §14.2）：脱敏照 v4 的样例逐条（hooks.md §14.1 的 `sanitizeHookDisplayText`），
//! 预览的截断，显示用的命令。

use super::*;
use crate::config::{CommandHook, HookShell};

#[test]
fn credentials_are_masked_like_v4() {
    for (text, masked) in [
        (
            "curl https://kevin:secret@example.com?api_key=abc --token xyz password=hunter2",
            "curl https://••••:••••@example.com?api_key=•••• --token •••• password=••••",
        ),
        (
            "Authorization: Bearer secret-token",
            "Authorization: Bearer ••••",
        ),
        (
            r#"curl -H "Authorization: Bearer secret-token" "https://example.com?api_key=abc" "password=hunter2""#,
            r#"curl -H "Authorization: Bearer ••••" "https://example.com?api_key=••••" "password=••••""#,
        ),
        (
            "GITHUB_TOKEN=abc FOO_GITHUB_TOKEN=def",
            "GITHUB_TOKEN=•••• FOO_GITHUB_TOKEN=••••",
        ),
        (
            "postgres://user:pass@host/db",
            "postgres://••••:••••@host/db",
        ),
        (
            r#"{"password":"secret","api_key":"abc"}"#,
            r#"{"password":••••,"api_key":••••}"#,
        ),
        (
            "/tmp/policy-plugin/hooks/check.mjs --token abc",
            "/tmp/policy-plugin/hooks/check.mjs --token ••••",
        ),
        // 没有凭据的文字原样（E2E HK-UP-01 的 stderr）
        (
            "python3: can't open file '/Users/dev/test/z-m/a.py': [Errno 2] No such file or directory",
            "python3: can't open file '/Users/dev/test/z-m/a.py': [Errno 2] No such file or directory",
        ),
    ] {
        assert_eq!(sanitize(text), masked, "{text}");
    }
}

#[test]
fn a_preview_is_trimmed_cut_at_4000_units_and_masked() {
    let preview = |text: &str| cut(text).map(|text| shown(&text));
    assert_eq!(preview("  \n "), None);
    assert_eq!(
        preview("  password=hunter2\n").as_deref(),
        Some("password=••••")
    );
    // 截到 4000 再接 `...`；脱敏之后再截到 4000（同 v4）：记号被截掉
    assert_eq!(
        cut(&"x".repeat(4001)).unwrap(),
        format!("{}...", "x".repeat(4000))
    );
    assert_eq!(preview(&"x".repeat(4001)).unwrap(), "x".repeat(4000));
    assert_eq!(preview(&"y".repeat(4000)).unwrap(), "y".repeat(4000));
}

fn exec(command: &str, args: Option<&[&str]>) -> HookCommand {
    HookCommand::Command(CommandHook {
        command: command.to_owned(),
        args: args.map(|args| args.iter().map(|arg| (*arg).to_owned()).collect()),
        shell: HookShell::Bash,
        timeout: None,
        condition: None,
        background: false,
        plugin: None,
        deny_on_error: false,
        status_message: None,
    })
}

#[test]
fn the_command_is_shown_like_v4() {
    // exec 形式：只含安全字符的原样，别的写成 JSON 字符串（v4 的 `quoteCommandPart`）
    assert_eq!(
        command(&exec(
            "/usr/local/bin/node",
            Some(&["/Users/dev/My Hooks/check.mjs", "trace.jsonl", "process"])
        )),
        r#"/usr/local/bin/node "/Users/dev/My Hooks/check.mjs" trace.jsonl process"#
    );
    // shell 形式是原文；凭据照样换掉
    assert_eq!(
        command(&exec("guard.sh --token abc", None)),
        "guard.sh --token ••••"
    );
}
