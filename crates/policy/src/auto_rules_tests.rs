use rust_cli_kernel_api::permission::syntax::parse_rule;

use super::*;

#[test]
fn interpreter_prefixes_and_whole_shell_rules_are_dangerous() {
    for text in [
        "Bash",
        "Bash()",
        "Bash(*)",
        "Bash(python:*)",
        "Bash(Python3 *)",
        "Bash(node*)",
        "Bash(npm run:*)",
        "Bash(sudo)",
        "Bash(python -c:*)",
        "Bash(bash -lc *)",
        "Agent",
        "Agent(Explore)",
        "Monitor",
    ] {
        assert!(dangerous(&parse_rule(text), false), "{text}");
    }
    for text in [
        "Bash(git status:*)",
        "Bash(npm run build:*)",
        "Bash(node script.js:*)",
        "Bash(python -m http.server:*)",
        "Bash(pythonista:*)",
        "Read",
        "Edit(src/**)",
        "WebFetch(domain:example.com)",
        "mcp__files",
    ] {
        assert!(!dangerous(&parse_rule(text), false), "{text}");
    }
    // classifyAllShell：全部 Bash 规则。
    assert!(dangerous(&parse_rule("Bash(git status:*)"), true));
    assert!(!dangerous(&parse_rule("Read"), true));
}
